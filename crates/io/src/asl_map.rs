//! Layer-style libraries (`.asl`, [`photosplice_psd::asl`]) as PhotoSplice styles: each style's
//! `Lefx` tree goes through the PSD layer-effects mapping ([`crate::effects_map::parse_lfx2`]),
//! its `blendOptions` give the blend mode and fill opacity it was saved with, and the library's
//! patterns become [`Pattern`]s for its pattern overlays.

use photosplice_color::BlendMode;
use photosplice_doc::{Effect, Pattern};
use photosplice_psd::asl::AslStyle;
use photosplice_psd::descriptor::{Value, VersionedDescriptor};

/// One style of a library, mapped.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub name: String,
    pub effects: Vec<Effect>,
    /// The blend mode saved with the style ("Include Layer Blending Options").
    pub blend: Option<BlendMode>,
    /// The fill opacity saved with it (`0..=1`).
    pub fill_opacity: Option<f32>,
}

/// The styles and patterns of an `.asl` file.
pub fn read_asl(bytes: &[u8]) -> Result<(Vec<Style>, Vec<Pattern>), String> {
    let file = photosplice_psd::asl::parse(bytes).map_err(|e| format!("not a readable Photoshop style library: {e}"))?;
    let patterns = file.patterns.iter().filter_map(crate::pattern_map::from_psd).collect();
    Ok((file.styles.iter().map(style).collect(), patterns))
}

fn style(s: &AslStyle) -> Style {
    // `Lefx` is an `lfx2` block's descriptor: wrapped as one (object-effects version 0).
    let effects = s
        .effects()
        .and_then(|fx| {
            let mut data = 0u32.to_be_bytes().to_vec();
            data.extend(VersionedDescriptor::new(fx.clone()).to_bytes());
            crate::effects_map::parse_lfx2(&data)
        })
        .map(|(_, fx)| fx)
        .unwrap_or_default();
    let opts = s.blend_options();
    let blend = opts.and_then(|o| match o.get("Md  ") {
        Some(Value::Enumerated { value, .. }) => crate::effects_map::blend_from_id(value.as_bytes()),
        _ => None,
    });
    let fill_opacity = opts.and_then(|o| crate::blocks::num(o.get("fillOpacity"))).map(|v| (v / 100.0).clamp(0.0, 1.0) as f32);
    let name = if s.name.trim().is_empty() { "Style".to_string() } else { s.name.trim().to_string() };
    Style { name, effects, blend, fill_opacity }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photosplice_psd::asl::{AslFile, write};
    use photosplice_psd::descriptor::{Descriptor, Id};

    #[test]
    fn a_library_maps_its_effects_and_blending_options() {
        // A drop shadow in Photoshop's own layer-effects form, written back as a style.
        let fx = crate::effects_map::write_lfx2(true, &[Effect::default_drop_shadow()]);
        let (lefx, _) = VersionedDescriptor::parse_prefix(&fx[4..]).unwrap();
        let opts = Descriptor::new("blendOptions")
            .with("Md  ", Value::Enumerated { type_id: Id::new("BlnM"), value: Id::new("Mltp") })
            .with("fillOpacity", Value::UnitFloat { unit: *b"#Prc", value: 40.0 });
        let st = Descriptor::new("Styl").with("Lefx", Value::Descriptor(lefx.descriptor)).with("blendOptions", Value::Descriptor(opts));
        let bytes = write(&AslFile { patterns: Vec::new(), styles: vec![AslStyle { name: "Shadowed".into(), id: "x".into(), style: st }] }).unwrap();
        let (styles, pats) = read_asl(&bytes).unwrap();
        assert!(pats.is_empty());
        assert_eq!(styles[0].name, "Shadowed");
        assert!(matches!(styles[0].effects.as_slice(), [Effect::DropShadow(_)]), "{:?}", styles[0].effects);
        assert_eq!(styles[0].blend, Some(BlendMode::Multiply));
        assert_eq!(styles[0].fill_opacity, Some(0.4));
        assert!(read_asl(b"nope").is_err());
    }
}
