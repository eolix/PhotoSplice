//! Files named by paths stored in a document: linked smart objects (and, as they gain one, any
//! other file a document names).
//!
//! Such a path is followed only through [`resolve`]: a relative path is taken from the document's
//! folder, and only local files, or files on the network share the document itself is on, are
//! opened. Paths to other computers, device paths and URLs are refused, so opening a document never
//! makes the app contact a server or device the user did not choose. A refused path is treated like
//! a missing file.
//!
//! The check is lexical (no I/O) and, apart from colons, gives the same answer on every platform:
//! Windows forms (`C:\`, `\\server\share`, `\\?\`, `\\.\`, reserved names such as `NUL`) are
//! recognised everywhere, and both `/` and `\` separate components. Windows reads a colon inside a
//! name as an alternate data stream and `name:…` as a URL for a protocol handler, so there any
//! scheme-like start and any colon inside a name is refused; on macOS and Linux a colon is an
//! ordinary character (Finder shows `/` in a name as `:`), so only URL-shaped links are refused.
//! A lexical check cannot see mapped drives, symlinks or junctions.

use std::fmt;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};

/// Why a stored path is refused by the lexical check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The path is on another computer, or on a network share other than the document's.
    Network,
    /// The path names a device rather than a file.
    Device,
    /// The link is a URL, not a file path.
    Url,
    /// The path is not usable (empty, control characters, climbing above its root, …).
    Invalid,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Network => "the path is on another computer",
            Self::Device => "the path names a device, not a file",
            Self::Url => "the link is not a file path",
            Self::Invalid => "the path is not valid",
        })
    }
}

impl std::error::Error for Refused {}

/// Why a stored path is not followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFollowed {
    /// The path is not a local file, or is not on the document's own share.
    Refused(Refused),
    /// The path is relative and the document has no file to take it from.
    NoDocumentFolder,
}

impl fmt::Display for NotFollowed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(r) => r.fmt(f),
            Self::NoDocumentFolder => f.write_str("the path is relative and the document has not been saved"),
        }
    }
}

impl std::error::Error for NotFollowed {}

/// The file to open for `stored`, a path or `file:` URL stored in a document, or why it is not
/// followed. `document` is the path of the document it is stored in, if the document has a file;
/// a relative `document` is taken from the working directory, where it was opened from.
pub fn resolve(stored: &str, document: Option<&Path>) -> Result<PathBuf, NotFollowed> {
    let document = document.map(|d| if d.is_relative() { std::path::absolute(d).unwrap_or_else(|_| d.to_path_buf()) } else { d.to_path_buf() });
    let path = lexical(stored, document.as_deref()).map_err(NotFollowed::Refused)?;
    // A path that is still relative would be taken from the working directory, which the
    // document does not choose.
    if path.is_relative() {
        return Err(if document.is_none() { NotFollowed::NoDocumentFolder } else { NotFollowed::Refused(Refused::Invalid) });
    }
    Ok(path)
}

/// Windows refuses colons in names and scheme-like starts; macOS and Linux allow them.
const COLON_IN_NAME_REFUSED: bool = cfg!(windows);

/// Schemes refused on every platform even when the rest does not look like a URL (ASCII case
/// ignored); any scheme starting with `ms-` (Windows app protocols) is refused too.
const URL_SCHEMES: [&str; 25] = [
    "about",
    "blob",
    "data",
    "file",
    "ftp",
    "ftps",
    "http",
    "https",
    "jar",
    "javascript",
    "mailto",
    "news",
    "nntp",
    "search-ms",
    "sftp",
    "shell",
    "smb",
    "sms",
    "ssh",
    "tel",
    "vbscript",
    "view-source",
    "webdav",
    "ws",
    "wss",
];

/// Where a parsed path starts.
#[derive(Debug, Clone)]
enum Root {
    /// `a\b`
    Relative,
    /// `\a` or `/a`
    Rooted,
    /// `C:a`
    DriveRelative(char),
    /// `C:\a`
    Drive(char),
    /// `\\?\C:\a`
    VerbatimDrive(char),
    /// `\\host\share\a`, or `\\?\UNC\host\share\a` when `verbatim`
    Share { host: String, share: String, verbatim: bool },
}

impl Root {
    fn is_absolute(&self) -> bool {
        !matches!(self, Self::Relative | Self::DriveRelative(_))
    }

    fn drive(&self) -> Option<char> {
        match self {
            Self::Drive(l) | Self::VerbatimDrive(l) => Some(*l),
            _ => None,
        }
    }
}

struct Parsed {
    root: Root,
    comps: Vec<String>,
}

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

/// Control characters, and separator lookalikes that code page best-fit mappings turn into `\` or
/// `/` when a path reaches an ANSI API.
fn is_bad_char(c: char) -> bool {
    c.is_control() || matches!(c, '\u{FF3C}' | '\u{FF0F}' | '\u{2215}' | '\u{29F5}')
}

/// The lexical resolution: `stored` made a path (joined to `document`'s folder when relative), or
/// refused. The result may still be relative when there is no document.
fn lexical(stored: &str, document: Option<&Path>) -> Result<PathBuf, Refused> {
    if stored.is_empty() || stored.chars().any(is_bad_char) {
        return Err(Refused::Invalid);
    }
    let plain = match stored.get(..5) {
        Some(scheme) if scheme.eq_ignore_ascii_case("file:") => file_url_path(stored.get(5..).unwrap_or_default())?,
        _ => stored.to_string(),
    };
    refuse_scheme(&plain)?;
    let link = parse(&plain, true)?;

    let doc = match document {
        // A relative link next to a document whose path is not UTF-8 (Unix only).
        Some(d) if d.to_str().is_none() && matches!(link.root, Root::Relative) => return join_native(d, &link.comps),
        Some(d) => d.to_str().map(|s| parse(s, false)),
        None => None,
    };
    let doc_share = match &doc {
        Some(Ok(Parsed { root: Root::Share { host, share, .. }, .. })) => Some((host.clone(), share.clone())),
        _ => None,
    };
    let folder = |comps: Vec<String>, link: Vec<String>| -> Vec<String> {
        let keep = comps.len().saturating_sub(1);
        comps.into_iter().take(keep).chain(link).collect()
    };
    let (root, comps) = match (link.root, doc) {
        (Root::Relative, Some(doc)) => {
            let doc = doc?;
            (doc.root, folder(doc.comps, link.comps))
        }
        (Root::Rooted, Some(Ok(doc))) if doc.root.is_absolute() => (doc.root, link.comps),
        (Root::DriveRelative(l), Some(Ok(doc))) if doc.root.drive().is_some_and(|d| d.eq_ignore_ascii_case(&l)) => (doc.root, folder(doc.comps, link.comps)),
        (root, _) => (root, link.comps),
    };
    let comps = normalize(&root, comps)?;
    if let Root::Share { host, share, .. } = &root
        && !doc_share.is_some_and(|(h, s)| h.eq_ignore_ascii_case(host) && s.eq_ignore_ascii_case(share))
    {
        return Err(Refused::Network);
    }
    Ok(render(&root, &comps))
}

/// Starts with an RFC 3986 scheme of two or more characters and a colon (so not a drive letter).
fn scheme(s: &str) -> Option<&str> {
    let (scheme, _) = s.split_once(':')?;
    let mut chars = scheme.chars();
    let ok =
        chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && scheme.len() >= 2 && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    ok.then_some(scheme)
}

/// Refuse device names used as a scheme (`NUL:`), and links that read as URLs.
fn refuse_scheme(s: &str) -> Result<(), Refused> {
    let Some(scheme) = scheme(s) else { return Ok(()) };
    if is_dos_device(scheme) {
        return Err(Refused::Device);
    }
    // URL-shaped: the first component ends with the colon (`https://h`, `shell:`,
    // `view-source:http://h`), or a `+`-part of the scheme is a known URL scheme.
    let first = s.split(is_sep).next().unwrap_or_default();
    let known = scheme.split('+').any(|part| {
        let part = part.to_ascii_lowercase();
        part.starts_with("ms-") || URL_SCHEMES.contains(&part.as_str())
    });
    if COLON_IN_NAME_REFUSED || first.ends_with(':') || known { Err(Refused::Url) } else { Ok(()) }
}

/// The path of a `file:` URL (`rest` is the part after `file:`): only an empty host, `localhost`
/// or a drive (`file://C:/x`) is local.
fn file_url_path(rest: &str) -> Result<String, Refused> {
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let decoded = percent_decode(rest);
    if decoded.is_empty() || decoded.chars().any(is_bad_char) {
        return Err(Refused::Invalid);
    }
    let mut chars = decoded.chars();
    let path = match (chars.next(), chars.next()) {
        (Some(a), Some(b)) if is_sep(a) && is_sep(b) => {
            let after = chars.as_str();
            let (host, path) = after.split_at(after.find(is_sep).unwrap_or(after.len()));
            if let Some(l) = drive_spec(host) {
                return Ok(format!("{l}:{path}"));
            }
            if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
                return Err(Refused::Network);
            }
            if path.is_empty() {
                return Err(Refused::Invalid);
            }
            path
        }
        _ => decoded.as_str(),
    };
    // `/C:/x` and `/C|/x` name the drive path `C:/x`.
    let mut chars = path.chars();
    if chars.next().is_some_and(is_sep) {
        let tail = chars.as_str();
        if let Some(l) = tail.get(..2).and_then(drive_spec)
            && tail.get(2..).is_some_and(|r| r.chars().next().is_none_or(is_sep))
        {
            return Ok(format!("{l}:{}", tail.get(2..).unwrap_or_default()));
        }
    }
    Ok(path.to_string())
}

/// `C:` or `C|` (the old URL spelling) → the drive letter.
fn drive_spec(s: &str) -> Option<char> {
    let mut c = s.chars();
    match (c.next(), c.next(), c.next()) {
        (Some(l), Some(':' | '|'), None) if l.is_ascii_alphabetic() => Some(l),
        _ => None,
    }
}

/// Percent-decode a `file:` URL path. A run of escapes that is not UTF-8 is read as Windows-1252,
/// which older Windows software wrote into file URLs.
fn percent_decode(s: &str) -> String {
    let hex = |b: Option<&u8>| b.and_then(|&b| char::from(b).to_digit(16)).and_then(|d| u8::try_from(d).ok());
    let mut out = String::with_capacity(s.len());
    let mut run: Vec<u8> = Vec::new();
    let mut rest = s;
    loop {
        let bytes = rest.as_bytes();
        if let (Some(b'%'), Some(h), Some(l)) = (bytes.first(), hex(bytes.get(1)), hex(bytes.get(2))) {
            run.push((h << 4) | l);
            rest = rest.get(3..).unwrap_or_default();
            continue;
        }
        match std::str::from_utf8(&run) {
            Ok(text) => out.push_str(text),
            Err(_) => out.extend(run.iter().map(|&b| windows_1252(b))),
        }
        run.clear();
        let mut chars = rest.chars();
        let Some(c) = chars.next() else { break };
        out.push(c);
        rest = chars.as_str();
    }
    out
}

/// A Windows-1252 byte as a char. Its unassigned bytes (0x81, 0x8D, 0x8F, 0x90, 0x9D) map to C1
/// controls, which are refused.
fn windows_1252(b: u8) -> char {
    const C1: [char; 32] = [
        '\u{20AC}', '\u{81}', '\u{201A}', '\u{192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{2C6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}',
        '\u{8D}', '\u{17D}', '\u{8F}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}', '\u{2DC}', '\u{2122}',
        '\u{161}', '\u{203A}', '\u{153}', '\u{9D}', '\u{17E}', '\u{178}',
    ];
    match b {
        0x80..=0x9F => C1.get(usize::from(b - 0x80)).copied().unwrap_or(char::REPLACEMENT_CHARACTER),
        _ => char::from(b),
    }
}

/// Split a plain path into its root and components. `check` applies the component rules (device
/// names, dot-and-space names, colons on Windows); the document's own path is not checked.
fn parse(s: &str, check: bool) -> Result<Parsed, Refused> {
    let mut chars = s.chars();
    let (first, second) = (chars.next(), chars.next());
    match (first, second) {
        (Some(a), Some(b)) if is_sep(a) && is_sep(b) => parse_double(s, check),
        (Some(a), _) if is_sep(a) => {
            let after = s.get(1..).unwrap_or_default();
            // `\??\` is the object-manager namespace.
            if after.strip_prefix("??").is_some_and(|r| r.chars().next().is_none_or(is_sep)) {
                return Err(Refused::Device);
            }
            Ok(Parsed { root: Root::Rooted, comps: comps(after, check)? })
        }
        (Some(l), Some(':')) if l.is_ascii_alphabetic() => {
            let rest = s.get(2..).unwrap_or_default();
            match rest.chars().next() {
                Some(c) if is_sep(c) => Ok(Parsed { root: Root::Drive(l), comps: comps(rest, check)? }),
                _ => Ok(Parsed { root: Root::DriveRelative(l), comps: comps(rest, check)? }),
            }
        }
        _ => Ok(Parsed { root: Root::Relative, comps: comps(s, check)? }),
    }
}

/// A path starting with two separators: a share, or a `\\?\`, `\\.\` or `//?/` prefix.
fn parse_double(s: &str, check: bool) -> Result<Parsed, Refused> {
    let t = s.get(2..).unwrap_or_default();
    let (first, after) = t.split_once(is_sep).unwrap_or((t, ""));
    if first != "?" && first != "." {
        return parse_share(t, false, check);
    }
    if let Some(v) = s.strip_prefix(r"\\?\") {
        return parse_verbatim(v, check);
    }
    // `\\.\`, and `\\?\` spelled with a `/`, which Windows normalises into the device namespace:
    // only its `UNC\host\share` form names a file.
    let (next, rest) = after.split_once(is_sep).unwrap_or((after, ""));
    if next.eq_ignore_ascii_case("UNC") {
        return parse_share(rest, false, check);
    }
    Err(Refused::Device)
}

/// `\\?\` paths (the part after the prefix): only `\` separates and nothing is normalised.
fn parse_verbatim(v: &str, check: bool) -> Result<Parsed, Refused> {
    let (first, rest) = match v.split_once('\\') {
        Some((f, r)) => (f, Some(r)),
        None => (v, None),
    };
    let drive = if first.ends_with(':') { drive_spec(first) } else { None };
    match (drive, rest) {
        // `\\?\C:` without a separator is the volume, not its root folder.
        (Some(l), Some(rest)) => Ok(Parsed { root: Root::VerbatimDrive(l), comps: verbatim_comps(rest, check)? }),
        (None, Some(rest)) if first.eq_ignore_ascii_case("UNC") => parse_share(rest, true, check),
        _ => Err(Refused::Device),
    }
}

/// `host\share\rest`: the part after the leading separators (or `\\?\UNC\`).
fn parse_share(t: &str, verbatim: bool, check: bool) -> Result<Parsed, Refused> {
    let split = |s: &str| -> (String, String) {
        let pair = if verbatim { s.split_once('\\') } else { s.split_once(is_sep) };
        let (a, b) = pair.unwrap_or((s, ""));
        (a.to_string(), b.to_string())
    };
    let (host, rest) = split(t);
    let (share, rest) = split(&rest);
    if host.is_empty() || share.is_empty() {
        return Err(Refused::Network);
    }
    let comps = if verbatim { verbatim_comps(&rest, check)? } else { comps(&rest, check)? };
    Ok(Parsed { root: Root::Share { host, share, verbatim }, comps })
}

fn check_comp(c: &str) -> Result<(), Refused> {
    if is_dos_device(c) {
        return Err(Refused::Device);
    }
    if is_dot_name(c) || (COLON_IN_NAME_REFUSED && c.contains(':')) {
        return Err(Refused::Invalid);
    }
    Ok(())
}

fn comps(s: &str, check: bool) -> Result<Vec<String>, Refused> {
    let mut out = Vec::new();
    for c in s.split(is_sep).filter(|c| !c.is_empty()) {
        if check {
            check_comp(c)?;
        }
        out.push(c.to_string());
    }
    Ok(out)
}

/// Components of a `\\?\` path: `.`, `..`, `/` and empty names (other than a trailing `\`) are
/// not usable, since the path is passed to the file system as is.
fn verbatim_comps(s: &str, check: bool) -> Result<Vec<String>, Refused> {
    let parts: Vec<&str> = s.split('\\').collect();
    let last = parts.len().saturating_sub(1);
    let mut out = Vec::with_capacity(parts.len());
    for (i, c) in parts.into_iter().enumerate() {
        if c.is_empty() && i == last {
            continue;
        }
        if c.is_empty() || c == "." || c == ".." || c.contains('/') {
            return Err(Refused::Invalid);
        }
        if check {
            check_comp(c)?;
        }
        out.push(c.to_string());
    }
    Ok(out)
}

/// A name of dots and spaces other than `.` and `..`: Windows trims trailing dots and spaces, so
/// `.. ` or `...` could act as `..`.
fn is_dot_name(c: &str) -> bool {
    !c.is_empty() && c != "." && c != ".." && c.chars().all(|ch| ch == '.' || ch == ' ')
}

/// A reserved DOS device name, with or without an extension, stream or trailing dots and spaces.
fn is_dos_device(c: &str) -> bool {
    let stem = c.split(['.', ':']).next().unwrap_or_default().trim_end_matches(' ');
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].iter().any(|n| stem.eq_ignore_ascii_case(n)) {
        return true;
    }
    let (Some(head), Some(tail)) = (stem.get(..3), stem.get(3..)) else { return false };
    let mut tail = tail.chars();
    (head.eq_ignore_ascii_case("COM") || head.eq_ignore_ascii_case("LPT")) && matches!((tail.next(), tail.next()), (Some('0'..='9' | '¹' | '²' | '³'), None))
}

/// Fold `.` and `..`. A rooted path may not climb above its root; a relative one keeps leading `..`.
fn normalize(root: &Root, comps: Vec<String>) -> Result<Vec<String>, Refused> {
    let mut out: Vec<String> = Vec::with_capacity(comps.len());
    for c in comps {
        match c.as_str() {
            "." => {}
            ".." if out.last().is_some_and(|l| l != "..") => {
                out.pop();
            }
            ".." if root.is_absolute() => return Err(Refused::Invalid),
            _ => out.push(c),
        }
    }
    Ok(out)
}

/// Windows-form results use `\` on every platform; the others the platform separator.
fn render(root: &Root, comps: &[String]) -> PathBuf {
    let (prefix, sep) = match root {
        Root::Relative if comps.is_empty() => (".".to_string(), MAIN_SEPARATOR),
        Root::Relative => (String::new(), MAIN_SEPARATOR),
        Root::Rooted => (MAIN_SEPARATOR.to_string(), MAIN_SEPARATOR),
        Root::DriveRelative(l) => (format!("{l}:"), '\\'),
        Root::Drive(l) => (format!("{l}:\\"), '\\'),
        Root::VerbatimDrive(l) => (format!(r"\\?\{l}:\"), '\\'),
        Root::Share { host, share, verbatim: false } => (format!(r"\\{host}\{share}\"), '\\'),
        Root::Share { host, share, verbatim: true } => (format!(r"\\?\UNC\{host}\{share}\"), '\\'),
    };
    PathBuf::from(prefix + &comps.join(&sep.to_string()))
}

/// A relative link next to a document whose path is not UTF-8.
fn join_native(doc: &Path, comps: &[String]) -> Result<PathBuf, Refused> {
    let mut p = doc.parent().map(Path::to_path_buf).unwrap_or_default();
    for c in comps {
        match c.as_str() {
            "." => {}
            ".." => {
                if !p.pop() {
                    return Err(Refused::Invalid);
                }
            }
            _ => p.push(c),
        }
    }
    Ok(p)
}

#[cfg(test)]
mod tests;
