//! A saved PDF's passwords and permissions (the PDF save options' Security): the Standard
//! security handler of ISO 32000-2 §7.6.4, at the encryption level of the file's compatibility,
//! as the Reference chooses it: 128-bit RC4 for PDF 1.4 and 1.5, 128-bit AES for PDF 1.6,
//! 256-bit AES for PDF 1.7 and 2.0.
//!
//! Applied last, to the finished file: every object of its newest revision is written again,
//! with its strings and streams enciphered, into one revision with an `/Encrypt` dictionary.
//! The files this rewrites are the ones the PDF writer and the incremental updates here produce
//! (classic cross-reference tables, no object streams).
//!
//! Passwords are used as typed: UTF-8 up to 127 bytes at 256-bit AES (without SASLprep's
//! normalisation, which only changes passwords with unusual Unicode), Latin-1 up to 32 bytes
//! below it, as the format defines.

use std::collections::{BTreeMap, HashSet};
use std::io::Write as _;

use aes::cipher::consts::U16;
use aes::cipher::{Array, BlockCipherEncrypt, KeyInit};
use md5::Md5;
use sha2::{Digest, Sha256, Sha384, Sha512};

use super::export::Compatibility;
use crate::IoError;

/// Printing Allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Printing {
    None,
    /// Low Resolution (150 dpi).
    Low,
    #[default]
    High,
}

impl Printing {
    /// Every choice: (it, its id in command params, its label in the PDF save options).
    pub const ALL: [(Printing, &'static str, &'static str); 3] =
        [(Printing::None, "none", "None"), (Printing::Low, "low", "Low Resolution (150 dpi)"), (Printing::High, "high", "High Resolution")];

    pub fn from_id(id: &str) -> Option<Printing> {
        Self::ALL.iter().find(|(_, i, _)| *i == id).map(|(c, _, _)| *c)
    }

    pub fn id(self) -> &'static str {
        Self::ALL.iter().find(|(c, _, _)| *c == self).map_or("high", |(_, i, _)| i)
    }
}

/// Changes Allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Changes {
    None,
    /// Inserting, deleting and rotating pages.
    Pages,
    /// Filling in form fields and signing.
    Forms,
    /// Commenting, filling in form fields and signing.
    Comments,
    /// Any except extracting pages.
    #[default]
    Any,
}

impl Changes {
    /// Every choice: (it, its id in command params, its label in the PDF save options).
    pub const ALL: [(Changes, &'static str, &'static str); 5] = [
        (Changes::None, "none", "None"),
        (Changes::Pages, "pages", "Inserting, Deleting, and Rotating Pages"),
        (Changes::Forms, "forms", "Filling in Form Fields and Signing"),
        (Changes::Comments, "comments", "Commenting, Filling in Form Fields, and Signing"),
        (Changes::Any, "any", "Any Except Extracting Pages"),
    ];

    pub fn from_id(id: &str) -> Option<Changes> {
        Self::ALL.iter().find(|(_, i, _)| *i == id).map(|(c, _, _)| *c)
    }

    pub fn id(self) -> &'static str {
        Self::ALL.iter().find(|(c, _, _)| *c == self).map_or("any", |(_, i, _)| i)
    }
}

/// The PDF save options' Security: a password to open the file, and one that guards what it
/// allows.
#[derive(Clone, PartialEq, Eq)]
pub struct Security {
    /// Document Open Password (empty: the file opens without one).
    pub open_password: String,
    /// Permissions Password (empty: nothing is restricted).
    pub permissions_password: String,
    pub printing: Printing,
    pub changes: Changes,
    /// Enable copying of text, images and other content.
    pub copy: bool,
    /// Enable text access of screen reader devices for the visually impaired.
    pub screen_readers: bool,
    /// Enable plaintext metadata (AES only: RC4 enciphers the metadata too).
    pub plaintext_metadata: bool,
}

impl Default for Security {
    fn default() -> Self {
        Security {
            open_password: String::new(),
            permissions_password: String::new(),
            printing: Printing::default(),
            changes: Changes::default(),
            copy: true,
            screen_readers: true,
            plaintext_metadata: false,
        }
    }
}

/// Never prints the passwords (settings are logged and shown in errors).
impl std::fmt::Debug for Security {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = |p: &str| if p.is_empty() { "none" } else { "set" };
        f.debug_struct("Security")
            .field("open_password", &set(&self.open_password))
            .field("permissions_password", &set(&self.permissions_password))
            .field("printing", &self.printing)
            .field("changes", &self.changes)
            .field("copy", &self.copy)
            .field("screen_readers", &self.screen_readers)
            .field("plaintext_metadata", &self.plaintext_metadata)
            .finish()
    }
}

impl Security {
    /// No password: the file is written unprotected.
    pub fn is_none(&self) -> bool {
        self.open_password.is_empty() && self.permissions_password.is_empty()
    }

    /// The permission flags (`/P`, ISO 32000-2 Table 22): everything without a permissions
    /// password; else what the choices allow, with the reserved bits set.
    pub fn flags(&self) -> i32 {
        let bit = |n: u32| 1u32 << (n - 1);
        if self.permissions_password.is_empty() {
            return !0b11;
        }
        let mut p: u32 = 0xFFFF_F0C0;
        p |= match self.printing {
            Printing::None => 0,
            Printing::Low => bit(3),
            Printing::High => bit(3) | bit(12),
        };
        p |= match self.changes {
            Changes::None => 0,
            Changes::Pages => bit(11),
            Changes::Forms => bit(9),
            Changes::Comments => bit(6) | bit(9),
            Changes::Any => bit(4) | bit(6) | bit(9) | bit(11),
        };
        if self.copy {
            p |= bit(5) | bit(10);
        }
        if self.screen_readers {
            p |= bit(10);
        }
        p as i32
    }
}

/// The cipher a compatibility level gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Rc4,
    Aes128,
    Aes256,
}

impl Method {
    fn of(c: Compatibility) -> Method {
        match c {
            Compatibility::Pdf14 | Compatibility::Pdf15 => Method::Rc4,
            Compatibility::Pdf16 => Method::Aes128,
            Compatibility::Pdf17 | Compatibility::Pdf20 => Method::Aes256,
        }
    }
}

fn bad(why: &str) -> IoError {
    IoError::Unsupported(format!("PDF: can't protect the file with a password ({why})"))
}

/// The 32 bytes a short password is padded with (ISO 32000-2 §7.6.4.3.2).
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08, 0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Random bytes, fetched from the system a page at a time (every string gets its own IV).
struct Random {
    buf: Vec<u8>,
}

impl Random {
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], IoError> {
        if self.buf.len() < N {
            let mut more = vec![0u8; 4096];
            getrandom::fill(&mut more).map_err(|_| bad("no random numbers on this system"))?;
            self.buf.extend_from_slice(&more);
        }
        let mut out = [0u8; N];
        let rest = self.buf.split_off(N);
        out.copy_from_slice(self.buf.get(..N).ok_or_else(|| bad("random numbers"))?);
        self.buf = rest;
        Ok(out)
    }
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    if key.is_empty() {
        return data.to_vec();
    }
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, usize::from(j));
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|b| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[usize::from(i)]);
            s.swap(usize::from(i), usize::from(j));
            b ^ s[usize::from(s[usize::from(i)].wrapping_add(s[usize::from(j)]))]
        })
        .collect()
}

/// CBC over whole blocks (`data`'s length a multiple of 16), without padding.
fn cbc<C: BlockCipherEncrypt<BlockSize = U16>>(c: &C, iv: [u8; 16], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut prev = iv;
    for chunk in data.chunks_exact(16) {
        let mut b = [0u8; 16];
        for ((o, x), p) in b.iter_mut().zip(chunk).zip(prev) {
            *o = x ^ p;
        }
        let mut block = Array::from(b);
        c.encrypt_block(&mut block);
        prev = block.into();
        out.extend_from_slice(&prev);
    }
    out
}

/// AES-CBC with PKCS #7 padding, the IV first (how PDF stores an AES string or stream).
fn aes_padded<C: BlockCipherEncrypt<BlockSize = U16>>(c: &C, iv: [u8; 16], data: &[u8]) -> Vec<u8> {
    let n = 16 - data.len() % 16;
    let mut padded = Vec::with_capacity(data.len() + n);
    padded.extend_from_slice(data);
    padded.resize(data.len() + n, n as u8);
    let mut out = iv.to_vec();
    out.extend_from_slice(&cbc(c, iv, &padded));
    out
}

fn aes128(key: &[u8]) -> Result<aes::Aes128, IoError> {
    aes::Aes128::new_from_slice(key).map_err(|_| bad("key"))
}

fn aes256(key: &[u8]) -> Result<aes::Aes256, IoError> {
    aes::Aes256::new_from_slice(key).map_err(|_| bad("key"))
}

/// The hash of a 256-bit AES password (ISO 32000-2 Algorithm 2.B).
fn hash_r6(password: &[u8], salt: &[u8], udata: &[u8]) -> Result<[u8; 32], IoError> {
    let mut k: Vec<u8> = Sha256::new().chain_update(password).chain_update(salt).chain_update(udata).finalize().to_vec();
    let mut round = 0u32;
    loop {
        let mut k1 = Vec::with_capacity(64 * (password.len() + k.len() + udata.len()));
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }
        let iv: [u8; 16] = k.get(16..32).and_then(|s| s.try_into().ok()).ok_or_else(|| bad("hash"))?;
        let e = cbc(&aes128(k.get(..16).ok_or_else(|| bad("hash"))?)?, iv, &k1);
        // The first 16 bytes as a big-endian number, modulo 3 (256 ≡ 1 mod 3: their sum's).
        let sum: u32 = e.iter().take(16).map(|&b| u32::from(b)).sum();
        k = match sum % 3 {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        round += 1;
        let last = u32::from(e.last().copied().unwrap_or(0));
        // At least 64 rounds, then until the last byte is at most the round number - 32
        // (by round 288 at the latest).
        if round >= 64 && last + 32 <= round {
            break;
        }
    }
    k.truncate(32);
    k.try_into().map_err(|_| bad("hash"))
}

/// A password as 256-bit AES reads it: UTF-8, at most 127 bytes (cut at a character).
fn utf8_password(p: &str) -> &[u8] {
    let mut end = p.len().min(127);
    while !p.is_char_boundary(end) {
        end -= 1;
    }
    p.as_bytes().get(..end).unwrap_or_default()
}

/// A password as RC4 and 128-bit AES read it: Latin-1, padded or cut to 32 bytes.
fn padded_password(p: &str) -> [u8; 32] {
    let bytes: Vec<u8> = p.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).take(32).collect();
    let mut out = PAD;
    for (o, b) in out.iter_mut().zip(bytes.iter().chain(PAD.iter())) {
        *o = *b;
    }
    out
}

/// What the `/Encrypt` dictionary holds, and the key the objects are enciphered with.
struct Keys {
    method: Method,
    file_key: Vec<u8>,
    dict: String,
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2 + 2);
    s.push('<');
    for x in b {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{x:02X}"));
    }
    s.push('>');
    s
}

fn md5_50(mut h: [u8; 16]) -> [u8; 16] {
    for _ in 0..50 {
        h = Md5::digest(h).into();
    }
    h
}

/// RC4 with `key`, then 19 more times with `key` XORed with 1…19 (Algorithms 3 and 5).
fn rc4_20(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = rc4(key, data);
    for i in 1..=19u8 {
        let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        out = rc4(&k, &out);
    }
    out
}

fn keys(sec: &Security, method: Method, id: &[u8], rng: &mut Random) -> Result<Keys, IoError> {
    let p = sec.flags();
    // Without a permissions password the open password is the owner's too: an empty owner
    // password would open the file without asking.
    let owner = if sec.permissions_password.is_empty() { &sec.open_password } else { &sec.permissions_password };
    let encrypt_metadata = method == Method::Rc4 || !sec.plaintext_metadata;
    match method {
        Method::Aes256 => {
            let file_key: [u8; 32] = rng.bytes()?;
            let (user, owner) = (utf8_password(&sec.open_password), utf8_password(owner));
            let (uv, uk): ([u8; 8], [u8; 8]) = (rng.bytes()?, rng.bytes()?);
            let mut u = hash_r6(user, &uv, &[])?.to_vec();
            u.extend_from_slice(&uv);
            u.extend_from_slice(&uk);
            let ue = cbc(&aes256(&hash_r6(user, &uk, &[])?)?, [0; 16], &file_key);
            let (ov, ok): ([u8; 8], [u8; 8]) = (rng.bytes()?, rng.bytes()?);
            let mut o = hash_r6(owner, &ov, &u)?.to_vec();
            o.extend_from_slice(&ov);
            o.extend_from_slice(&ok);
            let oe = cbc(&aes256(&hash_r6(owner, &ok, &u)?)?, [0; 16], &file_key);
            let mut perms = [0u8; 16];
            perms[..4].copy_from_slice(&p.to_le_bytes());
            perms[4..8].copy_from_slice(&[0xFF; 4]);
            perms[8] = if encrypt_metadata { b'T' } else { b'F' };
            perms[9..12].copy_from_slice(b"adb");
            perms[12..].copy_from_slice(&rng.bytes::<4>()?);
            let perms = cbc(&aes256(&file_key)?, [0; 16], &perms);
            let dict = format!(
                "<< /Filter /Standard /V 5 /R 6 /Length 256 /CF << /StdCF << /AuthEvent /DocOpen /CFM /AESV3 /Length 32 >> >> /StmF /StdCF /StrF /StdCF /O {} /U {} /OE {} /UE {} /Perms {} /P {p} /EncryptMetadata {encrypt_metadata} >>",
                hex(&o),
                hex(&u),
                hex(&oe),
                hex(&ue),
                hex(&perms)
            );
            Ok(Keys { method, file_key: file_key.to_vec(), dict })
        }
        Method::Rc4 | Method::Aes128 => {
            let user = padded_password(&sec.open_password);
            // Algorithm 3: the owner entry.
            let ok = md5_50(Md5::digest(padded_password(owner)).into());
            let o = rc4_20(&ok, &user);
            // Algorithm 2: the file key.
            let mut h = Md5::new();
            h.update(user);
            h.update(&o);
            h.update(p.to_le_bytes());
            h.update(id);
            if !encrypt_metadata {
                h.update([0xFF; 4]);
            }
            let file_key = md5_50(h.finalize().into());
            // Algorithm 5: the user entry.
            let mut u = rc4_20(&file_key, &Md5::new().chain_update(PAD).chain_update(id).finalize());
            u.extend_from_slice(&rng.bytes::<16>()?);
            let dict = if method == Method::Rc4 {
                format!("<< /Filter /Standard /V 2 /R 3 /Length 128 /O {} /U {} /P {p} >>", hex(&o), hex(&u))
            } else {
                format!(
                    "<< /Filter /Standard /V 4 /R 4 /Length 128 /CF << /StdCF << /AuthEvent /DocOpen /CFM /AESV2 /Length 16 >> >> /StmF /StdCF /StrF /StdCF /O {} /U {} /P {p} /EncryptMetadata {encrypt_metadata} >>",
                    hex(&o),
                    hex(&u)
                )
            };
            Ok(Keys { method, file_key: file_key.to_vec(), dict })
        }
    }
}

impl Keys {
    /// `data` of object `num` (generation `generation`) enciphered.
    fn encrypt(&self, num: u32, generation: u16, data: &[u8], rng: &mut Random) -> Result<Vec<u8>, IoError> {
        // Below 256-bit AES each object has its own key (Algorithm 1).
        let object_key = || {
            let mut h = Md5::new();
            h.update(&self.file_key);
            h.update(num.to_le_bytes().get(..3).unwrap_or_default());
            h.update(generation.to_le_bytes());
            if self.method == Method::Aes128 {
                h.update(b"sAlT");
            }
            <[u8; 16]>::from(h.finalize())
        };
        Ok(match self.method {
            Method::Rc4 => rc4(&object_key(), data),
            Method::Aes128 => aes_padded(&aes128(&object_key())?, rng.bytes()?, data),
            Method::Aes256 => aes_padded(&aes256(&self.file_key)?, rng.bytes()?, data),
        })
    }
}

/// A token of PDF syntax and where it lies.
#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Name(Vec<u8>),
    Num(Vec<u8>),
    Str(Vec<u8>),
    DictOpen,
    DictClose,
    /// `[`, `]`, `{`, `}`.
    Punct(u8),
    Kw(Vec<u8>),
}

fn is_white(c: u8) -> bool {
    matches!(c, 0 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn is_delim(c: u8) -> bool {
    matches!(c, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

#[derive(Clone, Copy)]
struct Lexer<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Lexer<'a> {
    fn at(b: &'a [u8], i: usize) -> Self {
        Lexer { b, i }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn skip_white(&mut self) {
        while let Some(c) = self.peek() {
            if is_white(c) {
                self.i += 1;
            } else if c == b'%' {
                while self.peek().is_some_and(|c| c != b'\n' && c != b'\r') {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }

    /// The next token with its start and end, after any whitespace and comments.
    fn next(&mut self) -> Result<Option<(Tok, usize, usize)>, IoError> {
        self.skip_white();
        let start = self.i;
        let Some(c) = self.peek() else { return Ok(None) };
        let tok = match c {
            b'(' => Tok::Str(self.literal()?),
            b'<' if self.b.get(self.i + 1) == Some(&b'<') => {
                self.i += 2;
                Tok::DictOpen
            }
            b'>' if self.b.get(self.i + 1) == Some(&b'>') => {
                self.i += 2;
                Tok::DictClose
            }
            b'<' => Tok::Str(self.hex()?),
            b'/' => {
                self.i += 1;
                Tok::Name(self.regular().to_vec())
            }
            b'[' | b']' | b'{' | b'}' => {
                self.i += 1;
                Tok::Punct(c)
            }
            b')' | b'>' => return Err(bad("unbalanced delimiter")),
            _ => {
                let word = self.regular().to_vec();
                if word.first().is_some_and(|c| c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.')) {
                    Tok::Num(word)
                } else {
                    Tok::Kw(word)
                }
            }
        };
        Ok(Some((tok, start, self.i)))
    }

    fn regular(&mut self) -> &'a [u8] {
        let start = self.i;
        while self.peek().is_some_and(|c| !is_white(c) && !is_delim(c)) {
            self.i += 1;
        }
        self.b.get(start..self.i).unwrap_or_default()
    }

    /// A literal string `( … )`, its escapes resolved.
    fn literal(&mut self) -> Result<Vec<u8>, IoError> {
        let mut out = Vec::new();
        let mut depth = 0usize;
        loop {
            let c = self.peek().ok_or_else(|| bad("unterminated string"))?;
            self.i += 1;
            match c {
                b'(' => {
                    if depth > 0 {
                        out.push(c);
                    }
                    depth += 1;
                }
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Ok(out);
                    }
                    out.push(c);
                }
                b'\\' => {
                    let e = self.peek().ok_or_else(|| bad("unterminated string"))?;
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.i += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xFF) as u8);
                        }
                        // A line continuation.
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.i += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                // An end of line in a string reads as a line feed.
                b'\r' => {
                    if self.peek() == Some(b'\n') {
                        self.i += 1;
                    }
                    out.push(b'\n');
                }
                _ => out.push(c),
            }
        }
    }

    /// A hexadecimal string `< … >`.
    fn hex(&mut self) -> Result<Vec<u8>, IoError> {
        self.i += 1;
        let mut digits = Vec::new();
        loop {
            let c = self.peek().ok_or_else(|| bad("unterminated string"))?;
            self.i += 1;
            match c {
                b'>' => break,
                c if c.is_ascii_hexdigit() => digits.push(c),
                c if is_white(c) => {}
                _ => return Err(bad("bad hexadecimal string")),
            }
        }
        if digits.len() % 2 == 1 {
            digits.push(b'0');
        }
        let nibble = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
        Ok(digits.chunks_exact(2).map(|p| nibble(p[0]) << 4 | nibble(p[1])).collect())
    }
}

fn number<T: std::str::FromStr>(t: &Tok) -> Option<T> {
    match t {
        Tok::Num(n) => std::str::from_utf8(n).ok()?.parse().ok(),
        _ => None,
    }
}

/// A trailer's entries this needs.
#[derive(Default)]
struct Trailer {
    root: Option<String>,
    info: Option<String>,
    id: Option<Vec<u8>>,
    prev: Option<usize>,
}

/// Reads a cross-reference section at `at` and its trailer: the objects it lists (number →
/// generation and offset; only those not already in `objects`) and the trailer's entries.
fn read_section(pdf: &[u8], at: usize, objects: &mut BTreeMap<u32, (u16, usize)>) -> Result<Trailer, IoError> {
    let mut lx = Lexer::at(pdf, at);
    let tok = |lx: &mut Lexer| -> Result<Tok, IoError> { lx.next()?.map(|(t, _, _)| t).ok_or_else(|| bad("truncated cross-reference table")) };
    if tok(&mut lx)? != Tok::Kw(b"xref".to_vec()) {
        return Err(bad("compressed cross-reference"));
    }
    loop {
        let t = tok(&mut lx)?;
        if t == Tok::Kw(b"trailer".to_vec()) {
            break;
        }
        let first: u32 = number(&t).ok_or_else(|| bad("bad cross-reference table"))?;
        let count: u32 = number(&tok(&mut lx)?).ok_or_else(|| bad("bad cross-reference table"))?;
        if count as usize > pdf.len() / 18 {
            return Err(bad("bad cross-reference table"));
        }
        for k in 0..count {
            let off: usize = number(&tok(&mut lx)?).ok_or_else(|| bad("bad cross-reference entry"))?;
            let generation: u16 = number(&tok(&mut lx)?).ok_or_else(|| bad("bad cross-reference entry"))?;
            let kind = tok(&mut lx)?;
            if kind == Tok::Kw(b"n".to_vec())
                && let Some(num) = first.checked_add(k)
            {
                objects.entry(num).or_insert((generation, off));
            } else if let Some(num) = first.checked_add(k) {
                // A free entry hides older revisions' object of that number too.
                objects.entry(num).or_insert((u16::MAX, 0));
            }
        }
    }
    let mut trailer = Trailer::default();
    if tok(&mut lx)? != Tok::DictOpen {
        return Err(bad("bad trailer"));
    }
    let mut depth = 1;
    while depth > 0 {
        let (t, _, _) = lx.next()?.ok_or_else(|| bad("truncated trailer"))?;
        match t {
            Tok::DictOpen => depth += 1,
            Tok::DictClose => depth -= 1,
            Tok::Name(n) if depth == 1 => {
                let mut ahead = lx;
                let refer = |ahead: &mut Lexer| -> Result<Option<String>, IoError> {
                    let (a, b) = (ahead.next()?.map(|t| t.0), ahead.next()?.map(|t| t.0));
                    let r = ahead.next()?.map(|t| t.0);
                    Ok(match (a.as_ref().and_then(number::<u32>), b.as_ref().and_then(number::<u16>), r) {
                        (Some(a), Some(b), Some(Tok::Kw(r))) if r == b"R" => Some(format!("{a} {b} R")),
                        _ => None,
                    })
                };
                match n.as_slice() {
                    b"Root" => trailer.root = refer(&mut ahead)?,
                    b"Info" => trailer.info = refer(&mut ahead)?,
                    b"Prev" => trailer.prev = ahead.next()?.and_then(|t| number(&t.0)),
                    b"ID" => {
                        if ahead.next()?.map(|t| t.0) == Some(Tok::Punct(b'['))
                            && let Some((Tok::Str(id), _, _)) = ahead.next()?
                        {
                            trailer.id = Some(id);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(trailer)
}

/// The finished file `pdf` protected by `sec`, at the encryption level of `compat`.
pub(super) fn encrypt(pdf: &[u8], sec: &Security, compat: Compatibility) -> Result<Vec<u8>, IoError> {
    let find_last = |needle: &[u8]| pdf.windows(needle.len()).rposition(|w| w == needle);
    let sx = find_last(b"startxref").ok_or_else(|| bad("no cross-reference"))?;
    let mut lx = Lexer::at(pdf, sx + 9);
    let mut at: usize = lx.next()?.and_then(|t| number(&t.0)).ok_or_else(|| bad("no cross-reference"))?;
    // The newest revision first: its trailer and objects win.
    let mut objects = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut newest: Option<Trailer> = None;
    while seen.insert(at) {
        let t = read_section(pdf, at, &mut objects)?;
        let prev = t.prev;
        match &mut newest {
            None => newest = Some(t),
            Some(n) => {
                n.root = n.root.take().or(t.root);
                n.info = n.info.take().or(t.info);
                n.id = n.id.take().or(t.id);
            }
        }
        match prev {
            Some(p) => at = p,
            None => break,
        }
    }
    let trailer = newest.ok_or_else(|| bad("no trailer"))?;
    let root = trailer.root.ok_or_else(|| bad("no catalog"))?;
    objects.retain(|_, (g, _)| *g != u16::MAX);

    let method = Method::of(compat);
    // The reader's RC4 and 128-bit AES take a password's UTF-8 bytes, the format its Latin-1
    // ones: only ASCII reads alike in both.
    if method != Method::Aes256 && !(sec.open_password.is_ascii() && sec.permissions_password.is_ascii()) {
        return Err(IoError::Unsupported(
            "PDF: below PDF 1.7 compatibility a password can only use ASCII letters, digits and punctuation; choose PDF 1.7 or later for others".into(),
        ));
    }
    let mut rng = Random { buf: Vec::new() };
    let id = match trailer.id {
        Some(id) if !id.is_empty() => id,
        _ => rng.bytes::<16>()?.to_vec(),
    };
    let keys = keys(sec, method, &id, &mut rng)?;
    let plaintext_metadata = method != Method::Rc4 && sec.plaintext_metadata;

    // The header as written (version and binary comment), else one for the version.
    let first = objects.values().map(|(_, off)| *off).min().unwrap_or(0);
    let mut out = match pdf.get(..first) {
        Some(h) if h.starts_with(b"%PDF-") && h.len() <= 1024 => h.to_vec(),
        _ => format!("%PDF-{}\n%\u{e2}\u{e3}\n", compat.id()).into_bytes(),
    };
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let mut offsets: BTreeMap<u32, (u16, usize)> = BTreeMap::new();
    for (&num, &(generation, off)) in &objects {
        let mut lx = Lexer::at(pdf, off);
        let header = (lx.next()?, lx.next()?, lx.next()?);
        match header {
            (Some((a, _, _)), Some((b, _, _)), Some((Tok::Kw(k), _, _))) if number(&a) == Some(num) && number(&b) == Some(generation) && k == b"obj" => {}
            _ => return Err(bad(&format!("object {num} isn't where the cross-reference says"))),
        }
        offsets.insert(num, (generation, out.len()));
        let _ = writeln!(out, "{num} {generation} obj");
        let body = object_body(pdf, lx.i, num, generation, &objects, &keys, plaintext_metadata, &mut rng)?;
        out.extend_from_slice(&body);
        out.extend_from_slice(b"\nendobj\n");
    }
    // The `/Encrypt` dictionary, itself never enciphered.
    let enc = objects.keys().next_back().map_or(1, |n| n + 1);
    offsets.insert(enc, (0, out.len()));
    let _ = write!(out, "{enc} 0 obj\n{}\nendobj\n", keys.dict);

    let size = enc + 1;
    let xref = out.len();
    let _ = write!(out, "xref\n0 {size}\n");
    // Free entries chained from object 0.
    let free: Vec<u32> = (1..size).filter(|n| !offsets.contains_key(n)).collect();
    let _ = writeln!(out, "{:010} 65535 f ", free.first().copied().unwrap_or(0));
    let mut free_next = free.iter().skip(1);
    for n in 1..size {
        match offsets.get(&n) {
            Some((generation, off)) => {
                let _ = writeln!(out, "{off:010} {generation:05} n ");
            }
            None => {
                let _ = writeln!(out, "{:010} 00001 f ", free_next.next().copied().unwrap_or(0));
            }
        }
    }
    let info = trailer.info.map(|i| format!(" /Info {i}")).unwrap_or_default();
    let id = hex(&id);
    let _ = write!(out, "trailer\n<< /Size {size} /Root {root}{info} /ID [{id} {id}] /Encrypt {enc} 0 R >>\nstartxref\n{xref}\n%%EOF\n");
    Ok(out)
}

/// The integer object `num` (an indirect `/Length`).
fn integer_object(pdf: &[u8], objects: &BTreeMap<u32, (u16, usize)>, num: u32) -> Option<usize> {
    let &(_, off) = objects.get(&num)?;
    let mut lx = Lexer::at(pdf, off);
    for _ in 0..3 {
        lx.next().ok()??;
    }
    number(&lx.next().ok()??.0)
}

/// Object `num`'s body from `start` (just after `obj`) to its `endobj`, with every string and its
/// stream enciphered.
#[allow(clippy::too_many_arguments)]
fn object_body(
    pdf: &[u8],
    start: usize,
    num: u32,
    generation: u16,
    objects: &BTreeMap<u32, (u16, usize)>,
    keys: &Keys,
    plaintext_metadata: bool,
    rng: &mut Random,
) -> Result<Vec<u8>, IoError> {
    let mut out = Vec::new();
    let mut lx = Lexer::at(pdf, start);
    let mut depth = 0usize;
    // The top dictionary's `/Length` (its span in `out` and its value) and whether it is
    // `/Type /Metadata`.
    let mut length: Option<(usize, usize, Option<usize>)> = None;
    let mut metadata = false;
    let mut last = start;
    loop {
        let (t, s, e) = lx.next()?.ok_or_else(|| bad(&format!("object {num} has no end")))?;
        // What lay between tokens (whitespace, comments) is kept.
        out.extend_from_slice(pdf.get(last..s).unwrap_or_default());
        last = e;
        match t {
            Tok::Kw(k) if k == b"endobj" => break,
            Tok::Kw(k) if k == b"stream" => {
                let mut ds = e;
                if pdf.get(ds) == Some(&b'\r') {
                    ds += 1;
                }
                if pdf.get(ds) == Some(&b'\n') {
                    ds += 1;
                }
                let n = length.and_then(|l| l.2);
                let ends_at = |de: usize| {
                    let mut l = Lexer::at(pdf, de);
                    l.skip_white();
                    pdf.get(l.i..).is_some_and(|r| r.starts_with(b"endstream")).then_some(l.i)
                };
                let (data_end, kw) = match n.and_then(|n| ds.checked_add(n)).and_then(|de| Some((de, ends_at(de)?))) {
                    Some(v) => v,
                    None => {
                        // A wrong length: the data runs to the `endstream`.
                        let rel = pdf.get(ds..).and_then(|r| r.windows(9).position(|w| w == b"endstream")).ok_or_else(|| bad("unterminated stream"))?;
                        let mut de = ds + rel;
                        if pdf.get(de.wrapping_sub(1)) == Some(&b'\n') {
                            de -= 1;
                        }
                        if pdf.get(de.wrapping_sub(1)) == Some(&b'\r') {
                            de -= 1;
                        }
                        (de.max(ds), ds + rel)
                    }
                };
                let data = pdf.get(ds..data_end).unwrap_or_default();
                let data = if plaintext_metadata && metadata { data.to_vec() } else { keys.encrypt(num, generation, data, rng)? };
                // The dictionary gets the enciphered length, as a direct number.
                let dict_end = out.iter().rposition(|&c| c == b'>').ok_or_else(|| bad("stream without a dictionary"))?;
                match length {
                    Some((a, b, _)) if b <= out.len() => {
                        out.splice(a..b, format!("/Length {}", data.len()).into_bytes());
                    }
                    _ => {
                        let at = dict_end.checked_sub(1).ok_or_else(|| bad("stream without a dictionary"))?;
                        out.splice(at..at, format!(" /Length {} ", data.len()).into_bytes());
                    }
                }
                out.extend_from_slice(b"stream\n");
                out.extend_from_slice(&data);
                out.extend_from_slice(b"\nendstream");
                last = kw + 9;
                lx = Lexer::at(pdf, last);
            }
            Tok::Str(bytes) => out.extend_from_slice(hex(&keys.encrypt(num, generation, &bytes, rng)?).as_bytes()),
            Tok::DictOpen => {
                depth += 1;
                out.extend_from_slice(b"<<");
            }
            Tok::DictClose => {
                depth = depth.saturating_sub(1);
                out.extend_from_slice(b">>");
            }
            Tok::Name(n) => {
                if depth == 1 && n == b"Length" {
                    let mut ahead = lx;
                    let a = ahead.next()?;
                    let mut end = a.as_ref().map_or(e, |t| t.2);
                    let mut value = a.as_ref().and_then(|t| number::<usize>(&t.0));
                    // `/Length n g R`: an indirect length.
                    let mut far = ahead;
                    if let (Some(b), Some((Tok::Kw(r), _, re))) = (far.next()?, far.next()?)
                        && r == b"R"
                        && number::<u16>(&b.0).is_some()
                    {
                        value = a.as_ref().and_then(|t| number::<u32>(&t.0)).and_then(|n| integer_object(pdf, objects, n));
                        end = re;
                        ahead = far;
                    }
                    let at = out.len();
                    out.extend_from_slice(pdf.get(s..end).unwrap_or_default());
                    length = Some((at, out.len(), value));
                    last = end;
                    lx = ahead;
                    continue;
                }
                if depth == 1 && n == b"Type" {
                    let mut ahead = lx;
                    if let Some((Tok::Name(v), _, _)) = ahead.next()? {
                        metadata |= v == b"Metadata";
                    }
                }
                out.extend_from_slice(pdf.get(s..e).unwrap_or_default());
            }
            _ => out.extend_from_slice(pdf.get(s..e).unwrap_or_default()),
        }
    }
    Ok(out)
}

/// The entries of a protected file's `/Encrypt` dictionary (names to numbers and strings), found
/// from the newest trailer's reference. That dictionary is never in an object stream, nor
/// enciphered, so it is read straight from the bytes.
fn encrypt_dict(bytes: &[u8]) -> Option<BTreeMap<Vec<u8>, Tok>> {
    let mut end = bytes.len();
    let (num, generation) = loop {
        let at = bytes.get(..end)?.windows(8).rposition(|w| w == b"/Encrypt")?;
        let mut lx = Lexer::at(bytes, at + 8);
        let (a, b, r) = (lx.next().ok()?, lx.next().ok()?, lx.next().ok()?);
        if let (Some((a, _, _)), Some((b, _, _)), Some((Tok::Kw(r), _, _))) = (a, b, r)
            && r == b"R"
            && let (Some(a), Some(b)) = (number::<u32>(&a), number::<u16>(&b))
        {
            break (a, b);
        }
        end = at;
    };
    let head = format!("{num} {generation} obj");
    let mut end = bytes.len();
    let at = loop {
        let at = bytes.get(..end)?.windows(head.len()).rposition(|w| w == head.as_bytes())?;
        if at == 0 || bytes.get(at - 1).is_some_and(|&c| is_white(c)) {
            break at;
        }
        end = at;
    };
    let mut lx = Lexer::at(bytes, at + head.len());
    if lx.next().ok()??.0 != Tok::DictOpen {
        return None;
    }
    let mut out = BTreeMap::new();
    let mut depth = 1;
    let mut key: Option<Vec<u8>> = None;
    while depth > 0 {
        match lx.next().ok()??.0 {
            Tok::DictOpen => depth += 1,
            Tok::DictClose => depth -= 1,
            Tok::Name(n) if depth == 1 && key.is_none() => key = Some(n),
            t => {
                if depth == 1
                    && let Some(k) = key.take()
                {
                    out.insert(k, t);
                }
                continue;
            }
        }
        if depth != 1 {
            key = None;
        }
    }
    Some(out)
}

/// Algorithm 7: the user password of an RC4 or 128-bit AES file (revisions 2–4) whose owner
/// password is `owner`, so the owner password opens it too (the reader only checks the user's).
/// `None` when `owner` isn't its owner password or the file isn't of those revisions.
pub(super) fn user_password(bytes: &[u8], owner: &str) -> Option<String> {
    let dict = encrypt_dict(bytes)?;
    let int = |k: &[u8]| dict.get(k).and_then(number::<u32>);
    let r = int(b"R")?;
    if !(2..=4).contains(&r) {
        return None;
    }
    let n = if r == 2 { 5 } else { (int(b"Length").unwrap_or(40) / 8).clamp(5, 16) as usize };
    let Some(Tok::Str(o)) = dict.get(b"O".as_slice()) else { return None };
    let mut h: [u8; 16] = Md5::digest(padded_password(owner)).into();
    if r >= 3 {
        for _ in 0..50 {
            h = Md5::digest(h.get(..n)?).into();
        }
    }
    let key = h.get(..n)?;
    let mut user = o.get(..32)?.to_vec();
    if r == 2 {
        user = rc4(key, &user);
    } else {
        for i in (0..=19u8).rev() {
            let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
            user = rc4(&k, &user);
        }
    }
    // What precedes the padding.
    let len = (0..=32).find(|&k| user.get(k..) == PAD.get(..32 - k))?;
    Some(user.get(..len)?.iter().map(|&c| char::from(c)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_matches_the_published_vector() {
        // "Key" / "Plaintext" from the original RC4 test vectors.
        assert_eq!(rc4(b"Key", b"Plaintext"), [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]);
    }

    #[test]
    fn literal_strings_resolve_their_escapes() {
        let mut lx = Lexer::at(b"(a\\(b\\)c (d) \\101\\n\\\nz)", 0);
        assert_eq!(lx.next().unwrap().unwrap().0, Tok::Str(b"a(b)c (d) A\nz".to_vec()));
        let mut lx = Lexer::at(b"<48 65 6C6C 6F7>", 0);
        assert_eq!(lx.next().unwrap().unwrap().0, Tok::Str(b"Hello\x70".to_vec()));
    }

    #[test]
    fn permissions_follow_the_choices() {
        let open = Security { open_password: "a".into(), ..Default::default() };
        assert_eq!(open.flags(), -4, "no permissions password: everything allowed");
        let none = Security { permissions_password: "b".into(), printing: Printing::None, changes: Changes::None, copy: false, screen_readers: false, ..Default::default() };
        assert_eq!(none.flags() as u32, 0xFFFF_F0C0);
        let low = Security { printing: Printing::Low, ..none.clone() };
        assert_eq!(low.flags() as u32 & 0x804, 0x004, "low-resolution printing: bit 3 without 12");
    }

    #[test]
    fn a_broken_file_is_an_error() {
        for pdf in [&b""[..], b"%PDF-1.7\nstartxref\n99999\n%%EOF", b"%PDF-1.7\nxref\n0 99999999\ntrailer\n<< >>\nstartxref\n9\n%%EOF"] {
            assert!(encrypt(pdf, &Security { open_password: "x".into(), ..Default::default() }, Compatibility::Pdf17).is_err());
        }
    }
}
