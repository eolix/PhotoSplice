//! Profiles installed by other software that PhotoSplice uses but never ships or embeds on its
//! own: Reference's default working CMYK, U.S. Web Coated (SWOP) v2, which it assumes for every
//! CMYK document without an embedded profile. Its licence forbids redistributing it, so it is
//! read from where Reference's installer or the operating system keeps it, once per process;
//! without it the built-in coated CMYK stands in ([`crate::Builtin::CoatedCmyk`]).
//!
//! The web build has no file system: [`swop_v2`] is always `None` there.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::Profile;

/// The description tag of Reference's default working CMYK profile.
pub const SWOP_V2_DESCRIPTION: &str = "U.S. Web Coated (SWOP) v2";
/// Its file name as Reference installs it.
pub const SWOP_V2_FILE: &str = "USWebCoatedSWOP.icc";

/// A profile found on disk.
#[derive(Clone, Debug)]
pub struct Found {
    pub path: PathBuf,
    pub profile: Arc<Profile>,
}

/// Folders scanned at most this deep below a search folder.
const MAX_DEPTH: usize = 2;
/// At most this many profile files are looked at per scan.
const MAX_FILES: usize = 2000;
/// Larger files are not output profiles.
const MAX_BYTES: u64 = 32 << 20;

/// Where Reference and the operating systems keep CMYK profiles, in the order they are searched:
/// Reference's own "Recommended" folders first, then the system colour folders.
pub fn search_dirs() -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let home = env("HOME").or_else(|| env("USERPROFILE"));
    let mut v = Vec::new();
    if cfg!(windows) {
        let x86 = env("CommonProgramFiles(x86)").unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)\Common Files"));
        let x64 = env("CommonProgramFiles").unwrap_or_else(|| PathBuf::from(r"C:\Program Files\Common Files"));
        for common in [x86, x64] {
            v.push(common.join(r"Adobe\Color\Profiles\Recommended"));
        }
        let root = env("SystemRoot").unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        v.push(root.join(r"System32\spool\drivers\color"));
    } else if cfg!(target_os = "macos") {
        v.push(PathBuf::from("/Library/Application Support/Adobe/Color/Profiles/Recommended"));
        if let Some(h) = &home {
            v.push(h.join("Library/Application Support/Adobe/Color/Profiles/Recommended"));
        }
        v.push(PathBuf::from("/Library/ColorSync/Profiles"));
        if let Some(h) = &home {
            v.push(h.join("Library/ColorSync/Profiles"));
        }
    } else {
        if let Some(h) = &home {
            v.push(h.join(".color/icc"));
            v.push(h.join(".local/share/color/icc"));
            v.push(h.join(".local/share/icc"));
        }
        v.push(PathBuf::from("/usr/share/color/icc"));
        v.push(PathBuf::from("/usr/local/share/color/icc"));
    }
    v
}

/// Reference's default working CMYK, when it is installed: [`find_swop_v2`] over [`search_dirs`],
/// read on the first call and cached for the process. Always `None` on the web.
pub fn swop_v2() -> Option<Found> {
    static CACHE: OnceLock<Option<Found>> = OnceLock::new();
    find_cached(&CACHE, search_dirs)
}

/// [`find_swop_v2`] over `dirs()`, once: later calls return the first answer without touching
/// the disk (`dirs` is not called again).
pub fn find_cached(cell: &OnceLock<Option<Found>>, dirs: impl FnOnce() -> Vec<PathBuf>) -> Option<Found> {
    cell.get_or_init(|| if cfg!(target_arch = "wasm32") { None } else { find_swop_v2(&dirs()) }).clone()
}

/// The U.S. Web Coated (SWOP) v2 profile in `dirs`: a CMYK profile named [`SWOP_V2_FILE`] in one
/// of them (first folder first), else the first CMYK profile below them whose description is
/// [`SWOP_V2_DESCRIPTION`]. Only CMYK output profiles are parsed (the header says which), so a
/// scan reads little of the other files. Unreadable files and folders are skipped.
#[cfg(not(target_arch = "wasm32"))]
pub fn find_swop_v2(dirs: &[PathBuf]) -> Option<Found> {
    let is_swop = |p: &Profile| p.description.trim().eq_ignore_ascii_case(SWOP_V2_DESCRIPTION);
    // The file Reference installs, by name: no folder walk.
    for d in dirs {
        let path = d.join(SWOP_V2_FILE);
        if let Some(profile) = read_cmyk(&path) {
            return Some(Found { path, profile: Arc::new(profile) });
        }
    }
    let mut files = Vec::new();
    for d in dirs {
        collect(d, 0, &mut files);
    }
    files.into_iter().find_map(|path| read_cmyk(&path).filter(is_swop).map(|p| Found { path, profile: Arc::new(p) }))
}

/// No file system on the web.
#[cfg(target_arch = "wasm32")]
pub fn find_swop_v2(_dirs: &[PathBuf]) -> Option<Found> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn collect(dir: &std::path::Path, depth: usize, files: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH || files.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    // A stable order, so the same file wins on every launch.
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if files.len() >= MAX_FILES {
            return;
        }
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            collect(&path, depth + 1, files);
        } else if path.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("icc") || x.eq_ignore_ascii_case("icm")) {
            files.push(path);
        }
    }
}

/// The CMYK output profile at `path`; `None` for anything else. The 128-byte header is read
/// first, so other profiles are not read whole.
#[cfg(not(target_arch = "wasm32"))]
fn read_cmyk(path: &std::path::Path) -> Option<Profile> {
    use std::io::Read;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES || meta.len() < 128 {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 128];
    f.read_exact(&mut header).ok()?;
    // Device class (bytes 12..16) and data colour space (16..20), ICC.1 7.2.
    if header.get(12..16) != Some(b"prtr".as_slice()) || header.get(16..20) != Some(b"CMYK".as_slice()) {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Profile::parse(&bytes).ok().filter(|p| p.color_space == crate::ColorSpace::Cmyk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{CmykParams, cmyk_profile};

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("photosplice-cms-installed-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("vendor")).unwrap();
        d
    }

    fn fake(description: &str) -> Vec<u8> {
        cmyk_profile(&CmykParams { description: description.into(), grid_a2b: 3, grid_b2a: 3, ..Default::default() }).to_bytes().to_vec()
    }

    #[test]
    fn finds_the_profile_by_its_description_below_a_folder() {
        let d = temp_dir("desc");
        std::fs::write(d.join("other.icc"), fake("Some Other CMYK")).unwrap();
        std::fs::write(d.join("vendor").join("Renamed.ICC"), fake(SWOP_V2_DESCRIPTION)).unwrap();
        std::fs::write(
            d.join("broken.icc"),
            b"not a profile at all, but long enough to have a header ................................................................",
        )
        .unwrap();
        std::fs::write(d.join("rgb.icc"), crate::Builtin::Srgb.profile().to_bytes().as_slice()).unwrap();
        let found = find_swop_v2(&[d.join("missing"), d.clone()]).unwrap();
        assert!(found.path.ends_with("vendor/Renamed.ICC"), "{:?}", found.path);
        assert_eq!(found.profile.description, SWOP_V2_DESCRIPTION);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_installed_file_name_wins_and_folders_are_searched_in_order() {
        let (a, b) = (temp_dir("name-a"), temp_dir("name-b"));
        // Matched by description in the first folder, by name in the second: the name wins.
        std::fs::write(a.join("x.icc"), fake(SWOP_V2_DESCRIPTION)).unwrap();
        std::fs::write(b.join(SWOP_V2_FILE), fake("Installed Under Its Own Name")).unwrap();
        let found = find_swop_v2(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(found.path, b.join(SWOP_V2_FILE));
        // A file of that name that is not a CMYK profile is not taken.
        std::fs::write(b.join(SWOP_V2_FILE), crate::Builtin::Srgb.profile().to_bytes().as_slice()).unwrap();
        assert_eq!(find_swop_v2(&[a.clone(), b.clone()]).unwrap().path, a.join("x.icc"));
        for d in [a, b] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn absent_profile_is_none() {
        let d = temp_dir("absent");
        std::fs::write(d.join("other.icc"), fake("Coated FOGRA39")).unwrap();
        assert!(find_swop_v2(std::slice::from_ref(&d)).is_none());
        assert!(find_swop_v2(&[]).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_search_is_cached() {
        let d = temp_dir("cache");
        std::fs::write(d.join(SWOP_V2_FILE), fake(SWOP_V2_DESCRIPTION)).unwrap();
        let cell = OnceLock::new();
        let first = find_cached(&cell, || vec![d.clone()]).unwrap();
        // Gone from disk, still the profile read the first time; the folders are not asked for.
        std::fs::remove_file(d.join(SWOP_V2_FILE)).unwrap();
        let again = find_cached(&cell, || panic!("searched twice")).unwrap();
        assert!(Arc::ptr_eq(&first.profile, &again.profile));
        // Not found the first time: not found later either.
        let empty = OnceLock::new();
        assert!(find_cached(&empty, || vec![d.clone()]).is_none());
        std::fs::write(d.join(SWOP_V2_FILE), fake(SWOP_V2_DESCRIPTION)).unwrap();
        assert!(find_cached(&empty, || vec![d.clone()]).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn search_dirs_name_the_platform_folders() {
        let dirs = search_dirs();
        assert!(!dirs.is_empty());
        if cfg!(target_os = "macos") {
            assert_eq!(dirs[0], PathBuf::from("/Library/Application Support/Adobe/Color/Profiles/Recommended"));
        }
    }
}
