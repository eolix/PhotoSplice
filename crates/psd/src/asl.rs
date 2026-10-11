//! Photoshop layer-style libraries (`.asl`): Window › Styles › Import Styles.
//!
//! Layout (version 3, as Photoshop writes it):
//!
//! * `u16` 2, the signature `8BSL`, `u16` 3;
//! * `u32` length, then the patterns the styles use: the same records as a PSD's `Patt` block
//!   ([`crate::patterns::parse_pattern_block`]);
//! * `u32` style count, then per style a `u32` length and, inside it, two versioned descriptors:
//!   the style's identity (`Nm  ` name, `Idnt` id) and the style itself (class `Styl`: its
//!   `Lefx`, the same layer-effects tree as a PSD's `lfx2` block, and `blendOptions`), padded
//!   to the stated length.

use crate::descriptor::{Descriptor, UnicodeString, Value, VersionedDescriptor};
use crate::error::{PsdError, Result};
use crate::patterns::{PsdPattern, parse_pattern_block, write_pattern_block};

const SIGNATURE: &[u8; 4] = b"8BSL";

/// One style of a library.
#[derive(Debug, Clone, PartialEq)]
pub struct AslStyle {
    /// Its name (`Nm  `).
    pub name: String,
    /// Its identifier (`Idnt`), empty when the file has none.
    pub id: String,
    /// The style descriptor (`Styl`): `Lefx`, `blendOptions`, `documentMode`, …
    pub style: Descriptor,
}

impl AslStyle {
    /// The layer-effects descriptor (`Lefx`), if the style has effects.
    pub fn effects(&self) -> Option<&Descriptor> {
        match self.style.get("Lefx") {
            Some(Value::Descriptor(d)) => Some(d),
            _ => None,
        }
    }

    /// The blending options descriptor (`blendOptions`), if the style was saved with them.
    pub fn blend_options(&self) -> Option<&Descriptor> {
        match self.style.get("blendOptions") {
            Some(Value::Descriptor(d)) => Some(d),
            _ => None,
        }
    }
}

/// A layer-style library: its styles and the patterns they use.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AslFile {
    /// The patterns its pattern overlays and bevel textures use.
    pub patterns: Vec<PsdPattern>,
    /// The styles, in panel order.
    pub styles: Vec<AslStyle>,
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.p.checked_add(n).ok_or_else(|| PsdError::invalid("ASL: length overflows"))?;
        let s = self.b.get(self.p..end).ok_or_else(|| PsdError::invalid("ASL: truncated"))?;
        self.p = end;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
}

fn text(d: &Descriptor, key: &str) -> String {
    match d.get(key) {
        Some(Value::Text(t)) => t.to_string_lossy().trim_end_matches('\0').to_string(),
        _ => String::new(),
    }
}

/// Parses an `.asl` file.
pub fn parse(data: &[u8]) -> Result<AslFile> {
    let mut r = Rd { b: data, p: 0 };
    let _prefix = r.u16()?;
    if r.take(4)? != SIGNATURE {
        return Err(PsdError::invalid("not a Photoshop style library (no 8BSL signature)"));
    }
    let version = r.u16()?;
    if version != 3 {
        return Err(PsdError::invalid(format!("ASL version {version}; version 3 is supported")));
    }
    let plen = r.u32()? as usize;
    let patterns = if plen == 0 { Vec::new() } else { parse_pattern_block(r.take(plen)?)? };
    let count = r.u32()?;
    // Each style takes at least its length field and two descriptors: cap by what's left.
    if u64::from(count) > (data.len() as u64 - r.p as u64) / 8 {
        return Err(PsdError::invalid(format!("ASL: {count} styles can't fit in the file")));
    }
    let mut styles = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let len = r.u32()? as usize;
        let body = r.take(len)?;
        let (info, used) = VersionedDescriptor::parse_prefix(body)?;
        let rest = body.get(used..).ok_or_else(|| PsdError::invalid("ASL: truncated style"))?;
        let (style, _) = VersionedDescriptor::parse_prefix(rest)?;
        styles.push(AslStyle { name: text(&info.descriptor, "Nm  "), id: text(&info.descriptor, "Idnt"), style: style.descriptor });
    }
    Ok(AslFile { patterns, styles })
}

/// Writes an `.asl` file (version 3).
pub fn write(file: &AslFile) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(SIGNATURE);
    out.extend_from_slice(&3u16.to_be_bytes());
    let pats = if file.patterns.is_empty() { Vec::new() } else { write_pattern_block(&file.patterns)? };
    let plen = u32::try_from(pats.len()).map_err(|_| PsdError::invalid("ASL: patterns too large"))?;
    out.extend_from_slice(&plen.to_be_bytes());
    out.extend_from_slice(&pats);
    let count = u32::try_from(file.styles.len()).map_err(|_| PsdError::invalid("ASL: too many styles"))?;
    out.extend_from_slice(&count.to_be_bytes());
    for s in &file.styles {
        let info = Descriptor::new("null").with("Nm  ", Value::Text(UnicodeString::new_nul(&s.name))).with("Idnt", Value::Text(UnicodeString::new_nul(&s.id)));
        let mut body = VersionedDescriptor::new(info).to_bytes();
        body.extend(VersionedDescriptor::new(s.style.clone()).to_bytes());
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        let len = u32::try_from(body.len()).map_err(|_| PsdError::invalid("ASL: style too large"))?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&body);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(name: &str) -> AslStyle {
        let fx = Descriptor::new("Lefx").with("Scl ", Value::UnitFloat { unit: *b"#Prc", value: 100.0 });
        AslStyle { name: name.into(), id: format!("id-{name}"), style: Descriptor::new("Styl").with("Lefx", Value::Descriptor(fx)) }
    }

    #[test]
    fn libraries_round_trip() {
        let file = AslFile { patterns: Vec::new(), styles: vec![style("Gold"), style("Glass Bevel")] };
        let back = parse(&write(&file).unwrap()).unwrap();
        assert_eq!(back, file);
        assert!(back.styles[0].effects().is_some());
        assert!(back.styles[0].blend_options().is_none());
    }

    #[test]
    fn damaged_files_are_errors() {
        let good = write(&AslFile { patterns: Vec::new(), styles: vec![style("A")] }).unwrap();
        assert!(parse(b"").is_err());
        assert!(parse(b"\0\x028BPS\0\x03").is_err(), "wrong signature");
        for cut in [4, 10, 14, good.len() - 3] {
            assert!(parse(&good[..cut]).is_err(), "cut at {cut}");
        }
        let mut huge = good.clone();
        huge[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(parse(&huge).is_err(), "absurd style count");
    }
}
