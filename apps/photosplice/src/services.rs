//! Native platform services: file dialogs (rfd), filesystem, codecs.

use photosplice_codecs::{ChannelLayout, EncodeOptions, Image, SampleType as CS};
use photosplice_color::{ColorMode, SampleType};
use photosplice_doc::{Document, Layer, Size};
use photosplice_format::RecoveryStore;
use photosplice_geom::Rect;
use photosplice_ui_egui::{FileDialogAnswer, FileDialogReply, FileDialogRequest, Recoverable, Services};
use std::cell::RefCell;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

#[cfg(test)]
mod recovery_tests;

/// Everything File › Open offers: PhotoSplice and PSD documents, PDFs, flat images, camera raw
/// files, and brushes (.abr), gradients (.grd) and styles (.asl), which go to the preset libraries.
const OPEN_EXTS: &[&str] = &[
    "pcraft", "psd", "psb", "psdt", "png", "jpg", "jpeg", "tif", "tiff", "webp", "gif", "bmp", "tga", "ico", "qoi", "exr", "hdr", "pbm", "pgm", "ppm", "pam",
    "pfm", "heic", "heif", "hif", "dng", "cr2", "cr3", "nef", "nrw", "arw", "pef", "orf", "rw2", "raf", "abr", "grd", "asl", "pdf",
];

/// Open dialog extensions that also match uppercase and mixed-case names (`IMG_0001.JPG`).
/// On Linux and the BSDs rfd turns each extension into a case-sensitive `*.ext` glob for the XDG
/// portal or zenity, so every letter becomes a character class, as the portal spec suggests
/// (`*.[iI][cC][oO]`). Windows and macOS take literal extensions and ignore case already.
fn open_filter_extensions(extensions: &[&str]) -> Vec<String> {
    let case_sensitive_globs = cfg!(all(unix, not(target_os = "macos")));
    let class = |c: char| if c.is_ascii_alphabetic() { format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase()) } else { c.to_string() };
    extensions.iter().map(|ext| if case_sensitive_globs { ext.chars().map(class).collect() } else { ext.to_string() }).collect()
}

fn open_filters() -> Vec<(&'static str, Vec<String>)> {
    vec![("All Formats", open_filter_extensions(OPEN_EXTS)), ("PhotoSplice", open_filter_extensions(&["pcraft"]))]
}

/// File › Save As formats: (filter name, extensions). The filter matching the suggested name's
/// extension comes first; anything else defaults to Photoshop. The native .pcraft format isn't
/// offered (it still opens, and an opened .pcraft file still saves in place).
const SAVE_FILTERS: &[(&str, &[&str])] = &[
    ("Photoshop", &["psd", "psb"]),
    ("PNG", &["png"]),
    ("JPEG", &["jpg"]),
    ("WebP", &["webp"]),
    ("TIFF", &["tif"]),
    ("Targa", &["tga"]),
    // Also in Photoshop's Save As list; ICO is PhotoSplice's own (#4357): up to 256×256, with alpha.
    ("BMP", &["bmp"]),
    ("GIF", &["gif"]),
    ("ICO", &["ico"]),
    ("OpenEXR", &["exr"]),
    ("PDF", &["pdf"]),
];

/// Non-document files the shell saves (Swatches panel exports): offered alone, so the dialog
/// never swaps their extension for a document format's.
const OTHER_SAVE_FILTERS: &[(&str, &[&str])] = &[("Color Swatches", &["aco"]), ("Swatch Exchange", &["ase"])];

/// Lists the save dialog's file types with the `suggested` type first (added if unlisted), so the dialog keeps that extension instead of .psd.
fn save_filters(suggested: &str) -> Vec<(String, Vec<String>)> {
    let ext = Path::new(suggested).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if let Some((name, exts)) = OTHER_SAVE_FILTERS.iter().find(|(_, exts)| exts.contains(&ext.as_str())) {
        return vec![(name.to_string(), exts.iter().map(|e| e.to_string()).collect())];
    }
    let mut v: Vec<(String, Vec<String>)> = SAVE_FILTERS.iter().map(|(name, exts)| (name.to_string(), exts.iter().map(|e| e.to_string()).collect())).collect();
    match v.iter().position(|(_, exts)| exts.contains(&ext)) {
        Some(i) => {
            let (name, mut exts) = v.remove(i);
            // Windows adds the first type's first extension to a bare name: `.psb`, not `.psd`.
            exts.retain(|e| *e != ext);
            exts.insert(0, ext);
            v.insert(0, (name, exts));
        }
        None if !ext.is_empty() => v.insert(0, (ext.to_ascii_uppercase(), vec![ext])),
        None => {}
    }
    v
}

/// `path` with the extension of the file type picked in the save dialog (`chosen`), where the
/// dialog left the name alone (#6121): the format follows the extension, and the GTK file chooser
/// keeps the typed name when another type is picked. Only a name that still has the extension the
/// dialog opened with (`opened_with`, `Untitled-1.psd` with PNG picked) or none gets the type's
/// extension, as PolyMeilex/rfd#341 does. An extension the user typed that PhotoSplice saves
/// (`art.png` with the PSD type untouched, the way to save a PNG before this) is theirs and stays,
/// and so does one of the type's own (`.psb` for PSD). One PhotoSplice doesn't save (`scan.2024`)
/// is part of the name, and the type's extension is added after it.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn with_chosen_type(path: &Path, opened_with: Option<&str>, chosen: &[String]) -> PathBuf {
    let Some(first) = chosen.first() else { return path.to_path_buf() };
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    let opened_ext = opened_with.and_then(|n| Path::new(n).extension()).and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    match ext {
        Some(ext) if chosen.contains(&ext) => path.to_path_buf(),
        Some(ext) if opened_ext.as_ref() == Some(&ext) => path.with_extension(first),
        Some(ext) if SAVE_FILTERS.iter().chain(OTHER_SAVE_FILTERS).any(|(_, exts)| exts.contains(&ext.as_str())) => path.to_path_buf(),
        _ => {
            let mut name = path.as_os_str().to_owned();
            name.push(".");
            name.push(first);
            PathBuf::from(name)
        }
    }
}

/// The name the save dialog starts with. Where the type picked gives a bare name its extension
/// (`adds_extension`), the dialog gets the stem and the suggested type (listed first by
/// [`save_filters`]) supplies `.psd`:
/// - On Windows a visible extension stays put when the user picks another type (with File name
///   extensions shown, `image.psd` saved as PhotoSplice stayed `image.psd`, #4394).
/// - On Linux the portal's dialog keeps the name as typed, and before returning asks whether to
///   replace a file of that name, so `test.psd` with PNG picked asked to replace test.psd, which
///   the save leaves alone (#6121). From the stem [`with_chosen_type`] gives the picked type's
///   extension, and a name that then exists is asked about under that name
///   (`portal_save::save_as`).
///
/// A stem that still has a dot (`scan.jpg.psd`: Windows could keep `scan.jpg`) or an upper-case
/// extension (the user's spelling) keeps the whole name. Elsewhere the whole name is kept: the
/// macOS panel has no type list, and rfd's Linux dialogs don't say which type was picked.
fn dialog_file_name(suggested: &str, adds_extension: bool) -> Option<String> {
    let path = Path::new(suggested);
    let name = path.file_name()?.to_string_lossy().into_owned();
    let (Some(stem), Some(ext)) = (path.file_stem(), path.extension().and_then(|e| e.to_str())) else { return Some(name) };
    let bare = adds_extension && Path::new(stem).extension().is_none() && !ext.is_empty() && ext == ext.to_ascii_lowercase();
    Some(if bare { stem.to_string_lossy().into_owned() } else { name })
}

/// Shows an Open or Save dialog without blocking the event loop (#673, #574): rfd's async dialog
/// is created here, on the main thread, with the window as its parent (a sheet on macOS, an owned
/// modal window elsewhere), and a helper thread waits for it and hands the answer to the app.
fn show_file_dialog(request: FileDialogRequest, parent: Option<&eframe::Frame>, reply: FileDialogReply) {
    let mut dialog = rfd::AsyncFileDialog::new();
    if let Some(parent) = parent {
        dialog = dialog.set_parent(parent);
    }
    let answer: Pin<Box<dyn Future<Output = Option<FileDialogAnswer>> + Send>> = match request {
        FileDialogRequest::Open { multiple, initial_dir, extensions } => {
            if let Some(dir) = initial_dir {
                dialog = dialog.set_directory(dir);
            }
            // A command-specific filter must not inherit the image formats used by File › Open.
            let dialog = if let Some(exts) = extensions {
                dialog.add_filter("Supported Files", &open_filter_extensions(&exts.iter().map(String::as_str).collect::<Vec<_>>()))
            } else {
                open_filters()
                    .into_iter()
                    .fold(dialog, |dialog, (name, extensions)| dialog.add_filter(name, &extensions))
                    .add_filter("Paint.NET", &open_filter_extensions(&["pdn"]))
            };
            if multiple {
                let picked = dialog.pick_files();
                Box::pin(async move { picked.await.map(|files| FileDialogAnswer::Paths(files.iter().map(path_of).collect())) })
            } else {
                let picked = dialog.pick_file();
                Box::pin(async move { picked.await.map(|file| FileDialogAnswer::Paths(vec![path_of(&file)])) })
            }
        }
        FileDialogRequest::Folder { initial_dir } => {
            if let Some(dir) = initial_dir {
                dialog = dialog.set_directory(dir);
            }
            let picked = dialog.pick_folder();
            Box::pin(async move { picked.await.map(|folder| FileDialogAnswer::FolderTo(path_of(&folder))) })
        }
        FileDialogRequest::Save { suggested } => {
            let filters = save_filters(&suggested);
            let name = dialog_file_name(&suggested, cfg!(windows));
            let dir = Path::new(&suggested).parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf);
            for (name, exts) in &filters {
                dialog = dialog.add_filter(name, exts);
            }
            if let Some(name) = &name {
                dialog = dialog.set_file_name(name);
            }
            if let Some(dir) = &dir {
                dialog = dialog.set_directory(dir);
            }
            // On Linux the portal's own dialog, which reports the file type picked (#6121); rfd if
            // the portal can't show it.
            #[cfg(target_os = "linux")]
            {
                use eframe::wgpu::rwh::HasWindowHandle;
                let parent = crate::portal_save::parent_id(parent.and_then(|p| p.window_handle().ok()).map(|h| h.as_raw()));
                let req = crate::portal_save::SaveRequest {
                    parent,
                    name: dialog_file_name(&suggested, true),
                    suggested: Path::new(&suggested).file_name().map(|n| n.to_string_lossy().into_owned()),
                    folder: dir,
                    filters,
                    current: Some(0),
                };
                Box::pin(async move {
                    let portal = zbus::blocking::connection::Builder::session()
                        .and_then(|b| b.build())
                        .map_err(|e| e.to_string())
                        .and_then(|conn| crate::portal_save::save_as(&conn, req));
                    match portal {
                        Ok(path) => path.map(|p| FileDialogAnswer::SaveTo(p.to_string_lossy().into_owned())),
                        Err(e) => {
                            log::info!("portal save dialog unavailable ({e}), using rfd");
                            dialog.save_file().await.map(|file| FileDialogAnswer::SaveTo(path_of(&file)))
                        }
                    }
                })
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = filters;
                let picked = dialog.save_file();
                Box::pin(async move { picked.await.map(|file| FileDialogAnswer::SaveTo(path_of(&file))) })
            }
        }
    };
    // Without the thread the reply is dropped unanswered, which the app takes as Cancel.
    if let Err(e) = std::thread::Builder::new().name("file dialog".into()).spawn(move || {
        // An empty answer from a system with no dialog backend isn't a Cancel (#3354).
        let answer = block_on(answer).or_else(|| crate::file_dialog_probe::known_problem().map(FileDialogAnswer::Unavailable));
        reply.send(answer)
    }) {
        log::error!("couldn't wait for the file dialog: {e}");
    }
}

fn path_of(file: &rfd::FileHandle) -> String {
    file.path().to_string_lossy().into_owned()
}

/// Runs `future` to completion on this thread, sleeping until it is woken.
fn block_on<T>(future: impl Future<Output = T>) -> T {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        // Spurious wake-ups just poll again.
        std::thread::park();
    }
}

/// Per-user settings directory: `PHOTOSPLICE_CONFIG_DIR`, else `<exe dir>/PhotoSpliceData` in
/// portable mode, else the platform convention. Everything the app persists lives under it; see
/// [`crate::app_dirs`].
pub fn config_dir() -> Option<PathBuf> {
    crate::app_dirs::config_dir()
}

pub fn prefs_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("preferences.json"))
}

/// The brush preset store (one file per preset group plus tip bitmaps; see
/// `photosplice_engine::preset_store`).
pub fn presets_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("Presets"))
}

fn recovery_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("Recovery"))
}

/// Write `bytes` crash-safely (temp file beside the target, fsync, rename, directory fsync; see
/// [`photosplice_format::atomic`]). Every document write (Save, Save As, Export, Save for Web) and
/// the preferences go through here, so a failed or interrupted save never destroys the old file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    photosplice_format::atomic_write(path, bytes).map_err(|e| e.to_string())
}

/// Where the bundled resources live, first match wins:
///
/// 1. `PHOTOSPLICE_RESOURCES`, to point a build at another copy;
/// 2. the app bundle, `Contents/Resources/resources` on macOS;
/// 3. a `resources` folder beside the executable (Windows, portable installs), or
///    `../share/photosplice/resources` relative to it (Linux and FreeBSD packages);
/// 4. `resources/` in the source checkout, for development builds.
pub fn resources_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PHOTOSPLICE_RESOURCES").map(PathBuf::from).filter(|p| p.is_dir()) {
        return Some(p);
    }
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidates = [
        exe_dir.join("../Resources/resources"),
        exe_dir.join("resources"),
        exe_dir.join("../share/photosplice/resources"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../resources"),
    ];
    candidates.into_iter().find(|p| p.is_dir()).and_then(|p| p.canonicalize().ok())
}

/// The preset libraries in `resources/` and the panel groups they become (see
/// `photosplice_ui_egui::bundled`). Licences are beside each file and in THIRD-PARTY-NOTICES.md.
pub fn bundled_libraries() -> Vec<photosplice_ui_egui::bundled::Library> {
    use photosplice_ui_egui::bundled::{Kind, Library};
    let Some(root) = resources_dir() else { return Vec::new() };
    [
        (Kind::Gradients, "uiGradients", "gradients/uigradients.grd"),
        (Kind::Shapes, "Font Awesome", "shapes/shapes.csh"),
        (Kind::Brushes, "Markers", "brushes/Markers.abr"),
        (Kind::Brushes, "Paintbrush Set", "brushes/Paintbrush_Set.abr"),
        (Kind::Brushes, "Pencil Scribbles", "brushes/Pencil_Scribbles.abr"),
        (Kind::Patterns, "Subtle Patterns", "patterns/patterns.pat"),
        (Kind::PatternsOnDemand, "Subtle Patterns (more)", "patterns/extra_patterns.pat"),
    ]
    .into_iter()
    .map(|(kind, group, rel)| (kind, group, root.join(rel)))
    .filter(|(_, _, p)| p.is_file())
    .map(|(kind, group, p)| Library { kind, group: group.to_string(), path: p.to_string_lossy().into_owned() })
    .collect()
}

/// Registers the measured lens profiles (`resources/lensfun/lens-database.json`), read the first
/// time Lens Correction needs them; without the file it keeps its generic profiles.
pub fn register_lens_profiles() {
    let Some(path) = resources_dir().map(|d| d.join("lensfun/lens-database.json")) else { return };
    photosplice_engine::lens_cmds::set_lens_database_loader(Box::new(move || match std::fs::read(&path) {
        Ok(b) => Some(b),
        Err(e) => {
            log::warn!("lens profiles not loaded from {}: {e}", path.display());
            None
        }
    }));
}

/// [`write_atomic`] reporting the fraction written (a background save's progress, #3389).
fn write_atomic_progress(path: &Path, bytes: &[u8], progress: &dyn Fn(f32)) -> Result<(), String> {
    photosplice_format::atomic_write_progress(path, bytes, progress).map_err(|e| e.to_string())
}

/// The IO options for a save or export: the shared encoder settings, plus PhotoSplice's PDF
/// options and the PSD's selected layers.
fn export_options(settings: &photosplice_ui_egui::ExportSettings) -> photosplice_io::ExportOptions {
    let mut opts = settings.io_options();
    opts.pdf_discard_editing = !settings.pdf_editing;
    opts.pdf = settings.pdf;
    opts.selected_layers = settings.selected_layers.clone();
    opts
}

/// The share of a save's progress bar that encoding fills; writing the file fills the rest. A
/// 24 MP, 5-layer PSD (572 MB) takes about 3 s to encode and 0.5 s or more to write and flush.
const ENCODE_SHARE: f32 = 0.85;

/// A save on a worker thread (`Services::save_file` and `Services::automation_save`, #2017):
/// encode, then write through `write`. Cancelling while encoding leaves the target untouched;
/// once writing starts the save can't be cancelled. The encoder's progress fills the first
/// [`ENCODE_SHARE`] of the job's progress, so the status bar and tab read "Saving NN%" (#3389).
fn save_through(
    doc: &Document,
    path: &str,
    settings: &photosplice_ui_egui::ExportSettings,
    ctl: &photosplice_ui_egui::jobs_ui::SaveCtl,
    write: impl Fn(&str, &[u8], &dyn Fn(f32)) -> Result<(), String>,
) -> Result<Vec<String>, String> {
    ctl.progress(0.0, "Encoding");
    let opts = export_options(settings);
    let cancelled = || ctl.check().is_err();
    let progress = |f: f32| ctl.progress(f * ENCODE_SHARE, "");
    let interrupt = photosplice_raster::Interrupt::new(&cancelled, &progress);
    let r = crate::crash_guard::guard("Save", || photosplice_io::export_with(doc, path, &opts, &interrupt).map_err(|e| e.to_string()))?;
    ctl.commit()?;
    ctl.progress(ENCODE_SHARE, "Writing");
    // The last bit is the flush and rename after the last chunk: 100% only once they're done.
    write(path, &r.bytes, &|f| ctl.progress(ENCODE_SHARE + (0.99 - ENCODE_SHARE) * f, ""))?;
    ctl.progress(1.0, "");
    Ok(r.warnings)
}

fn save_file(
    doc: &Document,
    path: &str,
    settings: &photosplice_ui_egui::ExportSettings,
    ctl: &photosplice_ui_egui::jobs_ui::SaveCtl,
) -> Result<Vec<String>, String> {
    save_through(doc, path, settings, ctl, |p, b, progress| write_atomic_progress(Path::new(p), b, progress))
}

type SharedRecovery = Rc<RefCell<Option<RecoveryStore>>>;

/// Run `f` on the recovery store (`Err` without a config directory). The service closures never
/// call each other, so the store is never borrowed twice.
fn with_store<R>(store: &SharedRecovery, f: impl FnOnce(&mut RecoveryStore) -> R) -> Result<R, String> {
    let mut slot = store.try_borrow_mut().map_err(|_| "crash recovery is busy".to_string())?;
    Ok(f(slot.as_mut().ok_or("no config directory")?))
}

/// Crash recovery: background incremental .pcraft autosaves into `dir` (`None`: no config
/// directory, so autosaves fail and nothing is recovered). Recovered documents keep their entries
/// until a newer autosave replaces them or they're saved or closed (see [`RecoveryStore`]).
fn recovery_services(dir: Option<PathBuf>) -> Services {
    let recover_dir = dir.clone();
    let store: SharedRecovery = Rc::new(RefCell::new(dir.map(RecoveryStore::new)));
    let (s1, s2, s4) = (store.clone(), store.clone(), store.clone());
    Services {
        autosave: Some(Box::new(move |doc: &Arc<Document>, revision: u64, path: Option<&str>| {
            with_store(&s1, |s| s.autosave_checked(doc, revision, path.map(str::to_string)))?
        })),
        autosave_results: Some(Box::new(move || with_store(&s4, |s| s.take_completed()).unwrap_or_default())),
        discard_autosave: Some(Box::new(move |id: u64| {
            let _ = with_store(&s2, |s| s.discard(id));
        })),
        recover: Some(Box::new(move || {
            let Some(dir) = recover_dir.as_ref() else { return Vec::new() };
            photosplice_format::list_recovery(dir)
                .into_iter()
                .map(|e| Recoverable {
                    key: e.info.key.clone(),
                    name: e.info.document_name.clone(),
                    path: e.info.original_path.clone(),
                    load: Box::new(move || photosplice_format::recover(&e).map_err(|e| e.to_string())),
                })
                .collect()
        })),
        adopt_autosave: Some(Box::new(move |id: u64, key: &str| {
            let _ = with_store(&store, |s| s.adopt(id, key));
        })),
        ..Default::default()
    }
}

/// The pixels to paste when the clipboard holds copied files rather than an image (Copy in
/// Files, Finder or Explorer puts paths on the clipboard, #338): the first file that decodes,
/// upright, as RGBA8. Files that aren't images are skipped after reading only their header.
fn image_from_files(paths: &[PathBuf]) -> Option<(u32, u32, Vec<u8>)> {
    use std::io::Read;
    paths.iter().find_map(|path| {
        // text/uri-list lines end in CRLF (RFC 2483, and GTK writes them so), but arboard splits
        // on LF only, so a path copied in GNOME Files arrives with a trailing '\r'.
        let path = path.to_str().map_or_else(|| path.clone(), |s| PathBuf::from(s.trim_end_matches('\r')));
        let mut head = Vec::with_capacity(256);
        std::fs::File::open(&path).ok()?.take(256).read_to_end(&mut head).ok()?;
        // TGA has no magic number and is only guessed from the header, so trust the extension
        // too before reading what may be a large non-image file.
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        match photosplice_codecs::detect(&head)? {
            photosplice_codecs::Format::Tga if photosplice_codecs::from_extension(ext) != Some(photosplice_codecs::Format::Tga) => return None,
            _ => {}
        }
        let img = photosplice_codecs::decode(&std::fs::read(&path).ok()?).ok()?;
        Some((img.width(), img.height(), img.to_rgba8()))
    })
}

/// Largest clipboard image payload read on Wayland (1 GiB, a 16K × 16K RGBA BMP), so no real
/// screenshot is turned away, while a broken or hostile source can't make us allocate without end.
#[cfg(target_os = "linux")]
const MAX_CLIPBOARD_IMAGE_BYTES: u64 = 4 * 16384 * 16384;

/// The clipboard image arboard can't read on Wayland (#2546): arboard 3.6 asks for `image/png`
/// only, so a screenshot tool or clipboard manager that offers just `image/jpeg`, `image/bmp` or
/// `image/webp` pasted nothing. Reads the other `image/*` types through the same data-control
/// protocol arboard uses, and decodes them with our own codecs. `None` off Wayland.
#[cfg(target_os = "linux")]
fn wayland_clipboard_image() -> Option<(u32, u32, Vec<u8>)> {
    use std::io::Read;
    use wl_clipboard_rs::paste::{ClipboardType, MimeType, Seat, get_contents, get_mime_types_ordered};
    let offers = get_mime_types_ordered(ClipboardType::Regular, Seat::Unspecified).ok()?;
    image_from_offers(&offers, |mime| {
        let (pipe, _) = get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Specific(mime)).ok()?;
        let mut bytes = Vec::new();
        pipe.take(MAX_CLIPBOARD_IMAGE_BYTES + 1).read_to_end(&mut bytes).ok()?;
        (bytes.len() as u64 <= MAX_CLIPBOARD_IMAGE_BYTES).then_some(bytes)
    })
}

/// The first clipboard offer, in the source's order, that `fetch` returns as a decodable image,
/// upright as RGBA8. Only `image/*` types are fetched, so text offers are never transferred.
#[cfg(any(target_os = "linux", test))]
fn image_from_offers(offers: &[String], mut fetch: impl FnMut(&str) -> Option<Vec<u8>>) -> Option<(u32, u32, Vec<u8>)> {
    offers.iter().filter(|mime| mime.starts_with("image/")).find_map(|mime| {
        let bytes = fetch(mime)?;
        // TGA has no magic number (see `image_from_files`), so trust it only when the type says so.
        if photosplice_codecs::detect(&bytes)? == photosplice_codecs::Format::Tga && !mime.contains("tga") {
            return None;
        }
        let img = photosplice_codecs::decode(&bytes).ok()?;
        Some((img.width(), img.height(), img.to_rgba8()))
    })
}

/// The shell command that starts this install under XWayland, where winit delivers file drops
/// (winit 0.30 has none on Wayland, #386). A Flatpak gets the X11 socket only without the Wayland
/// one, an AppImage relaunches by its own path, and anything else is `photosplice` on the PATH.
/// `None` outside Flatpak when there is no X server (`DISPLAY` unset) to run on.
#[cfg(any(target_os = "linux", test))]
pub fn xwayland_command(flatpak_id: Option<&str>, appimage: Option<&str>, x_display: bool) -> Option<String> {
    if let Some(id) = flatpak_id.filter(|id| !id.is_empty()) {
        return Some(format!("flatpak run --nosocket=wayland --socket=x11 {}", shell_word(id)));
    }
    let exe = appimage.filter(|path| !path.is_empty()).map_or_else(|| "photosplice".to_string(), shell_word);
    x_display.then(|| format!("WAYLAND_DISPLAY= {exe}"))
}

/// `s` as one POSIX shell word: bare when every character is safe there, else single-quoted.
#[cfg(any(target_os = "linux", test))]
fn shell_word(s: &str) -> String {
    if s.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+:,@".contains(&b)) { s.to_string() } else { format!("'{}'", s.replace('\'', r"'\''")) }
}

pub fn native(automation: Option<photosplice_automation::AuthorizedWorkspace>) -> Services {
    let clip: Rc<RefCell<Option<arboard::Clipboard>>> = Rc::default();
    let automation_read = automation.clone().map(|workspace| {
        Box::new(move |path: &str| {
            let bytes = workspace.read(path).map_err(|error| error.to_string())?;
            let name = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path).to_string();
            Ok((name, bytes))
        }) as photosplice_ui_egui::AutomationReadFn
    });
    let automation_write = automation.clone().map(|workspace| {
        Box::new(move |path: &str, bytes: &[u8]| workspace.write(path, bytes).map_err(|error| error.to_string())) as photosplice_ui_egui::AutomationWriteFn
    });
    let automation_open_pcraft = automation.clone().map(|workspace| {
        Box::new(move |path: &str| {
            let name = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path).to_string();
            let opened = crate::crash_guard::guard("Open", || workspace.open_pcraft(path).map_err(|e| e.to_string()))?;
            Ok(opened.map(|doc| (name, doc)))
        }) as photosplice_ui_egui::AutomationOpenPcraftFn
    });
    let automation_save = automation.clone().map(|workspace| {
        std::sync::Arc::new(move |doc: &Document, path: &str, settings: &photosplice_ui_egui::ExportSettings, ctl: &photosplice_ui_egui::jobs_ui::SaveCtl| {
            save_through(doc, path, settings, ctl, |p, b, _| workspace.write(p, b).map_err(|e| e.to_string()))
        }) as photosplice_ui_egui::SaveFileFn
    });
    let step: fn(&str, &serde_json::Value) -> photosplice_engine::Result<()> = photosplice_automation::workspace::authorize_desktop_engine_step;
    let automation_authorize = automation.is_some().then_some(step);
    let automation_command = automation.map(|_| {
        Box::new(|id: &str, params: &serde_json::Value| {
            photosplice_automation::workspace::authorize_desktop_engine_command(id, params).map_err(|error| error.to_string())
        }) as photosplice_ui_egui::AutomationCommandFn
    });
    Services {
        import: Some(Box::new(|name: &str, bytes: &[u8], max_svg_group_depth: usize| {
            crate::crash_guard::guard("Open", || {
                photosplice_io::import_with_svg_group_depth(name, bytes, max_svg_group_depth).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string())
            })
        })),
        export: Some(Box::new(|doc: &Document, path: &str, settings: &photosplice_ui_egui::ExportSettings| {
            let opts = export_options(settings);
            crate::crash_guard::guard("Export", || photosplice_io::export(doc, path, &opts).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string()))
        })),
        save_file: Some(Arc::new(save_file)),
        file_dialog: Some(Box::new(show_file_dialog)),
        // A blocking Save dialog for the callers that need the path at once (Save for Web).
        pick_save: Some(Box::new(|suggested: &str| {
            let mut d = rfd::FileDialog::new();
            for (name, exts) in save_filters(suggested) {
                d = d.add_filter(name, &exts);
            }
            if let Some(name) = dialog_file_name(suggested, cfg!(windows)) {
                d = d.set_file_name(name);
            }
            if let Some(dir) = Path::new(suggested).parent().filter(|p| !p.as_os_str().is_empty()) {
                d = d.set_directory(dir);
            }
            Some(d.save_file()?.to_string_lossy().to_string())
        })),
        pick_file: Some(Box::new(|filter: &str, exts: &[&str]| {
            Some(rfd::FileDialog::new().add_filter(filter, &open_filter_extensions(exts)).pick_file()?.to_string_lossy().to_string())
        })),
        resources_dir: resources_dir(),
        user_resources_dir: config_dir().map(|d| d.join("libraries")),
        recent_thumbs_dir: config_dir().map(|d| d.join("recent-thumbs")),
        gallery_thumbs_file: config_dir().map(|d| d.join("filter-gallery-thumbnails.json")),
        write: Some(Box::new(|path: &str, bytes: &[u8]| write_atomic(Path::new(path), bytes))),
        automation_read,
        automation_write,
        automation_open_pcraft,
        automation_save,
        automation_command,
        automation_authorize,
        encode_png: Some(Box::new(|w, h, rgba| {
            let img = Image::from_u8(w, h, ChannelLayout::Rgba, rgba.to_vec()).map_err(|e| e.to_string())?;
            photosplice_codecs::encode(&img, photosplice_codecs::Format::Png, &EncodeOptions::default()).map_err(|e| e.to_string())
        })),
        inbox: None,
        open_url: Some(Box::new(|url: &str| open::that(url).map_err(|e| e.to_string()))),
        beep: Some(platform_beep),
        clipboard_set_image: Some({
            let clip = clip.clone();
            Box::new(move |w: u32, h: u32, px: &[u8]| {
                let mut slot = clip.try_borrow_mut().map_err(|_| "clipboard is busy".to_string())?;
                let cb = match slot.as_mut() {
                    Some(c) => c,
                    None => slot.insert(arboard::Clipboard::new().map_err(|e| e.to_string())?),
                };
                cb.set_image(arboard::ImageData { width: w as usize, height: h as usize, bytes: std::borrow::Cow::Borrowed(px) }).map_err(|e| e.to_string())
            })
        }),
        clipboard_get_image: Some({
            let clip = clip.clone();
            Box::new(move || {
                let mut slot = clip.try_borrow_mut().ok()?;
                let cb = match slot.as_mut() {
                    Some(c) => c,
                    None => slot.insert(arboard::Clipboard::new().ok()?),
                };
                // Copied files first: Finder also puts the file's icon on the clipboard as an
                // image, which would otherwise paste instead of the file.
                if let Some(img) = cb.get().file_list().ok().and_then(|paths| image_from_files(&paths)) {
                    return Some(img);
                }
                if let Ok(img) = cb.get_image() {
                    return Some((img.width as u32, img.height as u32, img.bytes.into_owned()));
                }
                #[cfg(target_os = "linux")]
                return wayland_clipboard_image();
                #[cfg(not(target_os = "linux"))]
                None
            })
        }),
        load_prefs: Some(Box::new(|| std::fs::read_to_string(prefs_file()?).ok())),
        photoshop_shortcuts: Some(Box::new(|| crate::photoshop_settings::live_keyboard_shortcuts(&|k| std::env::var_os(k)))),
        save_prefs: Some(Box::new(|text: &str| write_atomic(&prefs_file().ok_or("no config directory")?, text.as_bytes()))),
        system_theme: crate::appearance::service(),
        append_text: Some(Box::new(|path: &str, text: &str| {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| e.to_string())?;
            f.write_all(text.as_bytes()).map_err(|e| e.to_string())
        })),
        // Set by main once the Apple-event handlers are connected (macOS).
        os_events: None,
        // Set by main, which starts loading the store before the window opens.
        preset_store: None,
        ..recovery_services(recovery_dir())
    }
}

/// Preferences ▸ General ▸ Beep When Done: the platform's default information beep.
fn platform_beep() {
    #[cfg(target_os = "windows")]
    {
        let _ = winsafe::MessageBeep(winsafe::co::MBP::OK);
    }
    #[cfg(not(target_os = "windows"))]
    {
        // No beep API among this app's other platforms' dependencies: write the terminal bell
        // (audible when launched from a terminal, which is the same cue macOS terminals use).
        use std::io::Write as _;
        let _ = std::io::stdout().write_all(b"\x07").and_then(|()| std::io::stdout().flush());
    }
}

/// Flat-image import via photosplice-codecs (kept for reference/tests; the app uses photosplice-io).
#[allow(dead_code)]
pub fn import_flat(name: &str, bytes: &[u8]) -> Result<Document, String> {
    let img = photosplice_codecs::decode(bytes).map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    let depth = match img.sample_type() {
        CS::U8 => SampleType::U8,
        CS::U16 => SampleType::U16,
        _ => SampleType::F32,
    };
    let gray = matches!(img.layout(), ChannelLayout::Gray | ChannelLayout::GrayA);
    let cmyk = matches!(img.layout(), ChannelLayout::Cmyk | ChannelLayout::CmykA);
    let mode = if gray {
        ColorMode::Grayscale
    } else if cmyk {
        ColorMode::Cmyk
    } else {
        ColorMode::Rgb
    };
    let target = match mode {
        ColorMode::Grayscale => ChannelLayout::GrayA,
        ColorMode::Cmyk => ChannelLayout::CmykA,
        _ => ChannelLayout::Rgba,
    };
    let sample = match depth {
        SampleType::U8 => CS::U8,
        SampleType::U16 => CS::U16,
        SampleType::F32 => CS::F32,
    };
    let conv = img.convert(target, sample);
    let stem = std::path::Path::new(name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or(name.to_string());
    let mut doc = Document::new(stem, Size::new(w, h), mode, depth);
    doc.icc_profile = img.icc.clone().map(std::sync::Arc::new);
    if let Some((x, _)) = img.meta.dpi {
        doc.resolution_dpi = x;
    }
    let mut layer = Layer::raster("Background", doc.pixel_format());
    let data = conv.to_normalized();
    layer.surface_mut().ok_or("new raster layer has no pixels")?.write_region(Rect::from_xywh(0, 0, w, h), &data);
    doc.layers.push(layer);
    Ok(doc)
}

#[allow(dead_code)]
pub fn export_flat(doc: &Document, path: &str) -> Result<Vec<u8>, String> {
    let format = photosplice_codecs::from_extension(path).ok_or_else(|| format!("unknown file type for {path}"))?;
    let buf = photosplice_compose::flatten(doc);
    let (w, h) = (buf.rect.width(), buf.rect.height());
    let data: Vec<f32> = buf.px.iter().flat_map(|p| *p).collect();
    let img = match doc.depth {
        SampleType::U8 => Image::from_u8(w, h, ChannelLayout::Rgba, buf.to_rgba8().pixels),
        SampleType::U16 => Image::from_u16(w, h, ChannelLayout::Rgba, &data.iter().map(|v| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).collect::<Vec<_>>()),
        SampleType::F32 => Image::from_f32(w, h, ChannelLayout::Rgba, &data),
    }
    .map_err(|e| e.to_string())?;
    let img = match &doc.icc_profile {
        Some(icc) if doc.mode == ColorMode::Rgb => img.with_icc(Some((**icc).clone())),
        _ => img,
    };
    photosplice_codecs::encode(&img, format, &EncodeOptions::default()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use photosplice_engine::Session;
    use photosplice_format::list_recovery;
    use photosplice_ui_egui::{PhotospliceApp, prefs_ui};
    use serde_json::{Value, json};

    /// #6121: the file type picked in the save dialog gives the name its extension where the
    /// dialog left the one it opened with, and only there.
    #[test]
    fn the_picked_file_type_gives_the_name_its_extension() {
        let exts = |e: &[&str]| e.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (png, psd) = (exts(&["png"]), exts(&["psd", "psb"]));
        let opened = Some("Untitled-1.psd");
        let cases = [
            // PNG picked, the name as the dialog opened (or renamed, its extension kept).
            ("/pics/Untitled-1.psd", opened, &png, "/pics/Untitled-1.png"),
            ("/pics/art.psd", opened, &png, "/pics/art.png"),
            ("/pics/ART.PSD", opened, &png, "/pics/ART.png"),
            // No extension: the type's. One PhotoSplice doesn't save is part of the name.
            ("/pics/art", opened, &png, "/pics/art.png"),
            ("/pics/scan.2024", opened, &png, "/pics/scan.2024.png"),
            // Typed, an extension PhotoSplice saves is the user's: kept, whatever the type says
            // (the PSD type untouched, or PNG picked and .jpg typed).
            ("/pics/art.png", opened, &psd, "/pics/art.png"),
            ("/pics/art.jpg", opened, &png, "/pics/art.jpg"),
            // Save As doesn't offer .pcraft: a typed one is part of the name.
            ("/pics/art.pcraft", opened, &png, "/pics/art.pcraft.png"),
            // Already the type's: kept, including its other extensions.
            ("/pics/art.png", opened, &png, "/pics/art.png"),
            ("/pics/art.PNG", opened, &png, "/pics/art.PNG"),
            ("/pics/big.psb", opened, &psd, "/pics/big.psb"),
            // The dialog opened on a .pcraft: that is the extension the type replaces.
            ("/pics/art.pcraft", Some("art.pcraft"), &png, "/pics/art.png"),
            ("/pics/art.psd", Some("art.pcraft"), &png, "/pics/art.psd"),
        ];
        for (typed, opened, chosen, want) in cases {
            assert_eq!(with_chosen_type(Path::new(typed), opened, chosen), PathBuf::from(want), "{typed} (opened {opened:?})");
        }
        // No type picked: the name as typed.
        assert_eq!(with_chosen_type(Path::new("/pics/art.psd"), opened, &[]), PathBuf::from("/pics/art.psd"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn open_filters_cover_uppercase_and_mixed_case_extensions() {
        // Matches `text` against a glob of literals and `[..]` classes, as the portal would after `*.`.
        fn matches(glob: &str, text: &str) -> bool {
            let mut text = text.chars();
            let mut glob = glob.chars();
            while let Some(g) = glob.next() {
                let class: String = if g == '[' { glob.by_ref().take_while(|&c| c != ']').collect() } else { g.to_string() };
                if !text.next().is_some_and(|t| class.contains(t)) {
                    return false;
                }
            }
            text.next().is_none()
        }
        assert_eq!(open_filter_extensions(&["cr2", "pcraft"]), ["[cC][rR]2", "[pP][cC][rR][aA][fF][tT]"]);
        let globs = open_filter_extensions(OPEN_EXTS);
        assert_eq!(globs.len(), OPEN_EXTS.len());
        for (ext, glob) in OPEN_EXTS.iter().zip(&globs) {
            // Every casing: lower, UPPER, and alternating both ways (JpG, jPg).
            let alternating = |upper_first: bool| -> String {
                ext.chars().enumerate().map(|(i, c)| if (i % 2 == 0) == upper_first { c.to_ascii_uppercase() } else { c }).collect()
            };
            for name in [ext.to_string(), ext.to_ascii_uppercase(), alternating(true), alternating(false)] {
                assert!(matches(glob, &name), "{glob} should match .{name}");
            }
            assert!(!matches(glob, &format!("{ext}x")) && !matches(glob, &ext[..ext.len() - 1]), "{glob} matches only .{ext}");
        }
        assert!(!globs.iter().any(|glob| matches(glob, "txt")));
    }

    #[cfg(not(all(unix, not(target_os = "macos"))))]
    #[test]
    fn open_filters_keep_literal_extensions_on_windows_and_macos() {
        assert_eq!(open_filter_extensions(OPEN_EXTS), OPEN_EXTS);
        assert_eq!(open_filter_extensions(&["pcraft"]), ["pcraft"]);
    }

    #[test]
    fn xwayland_command_matches_how_photosplice_was_installed() {
        // Flatpak: WAYLAND_DISPLAY doesn't reach the sandbox's socket choice; flatpak's flags do.
        let flatpak = xwayland_command(Some("io.github.eolix.PhotoSplice"), None, false);
        assert_eq!(flatpak.as_deref(), Some("flatpak run --nosocket=wayland --socket=x11 io.github.eolix.PhotoSplice"));
        // AppImage: its own path, quoted for the shell (spaces, quotes).
        let appimage = xwayland_command(None, Some("/home/me/My Apps/it's.AppImage"), true);
        assert_eq!(appimage.as_deref(), Some(r"WAYLAND_DISPLAY= '/home/me/My Apps/it'\''s.AppImage'"));
        let bare = xwayland_command(None, Some("/opt/photosplice-0.3.0-linux-x86_64.AppImage"), true);
        assert_eq!(bare.as_deref(), Some("WAYLAND_DISPLAY= /opt/photosplice-0.3.0-linux-x86_64.AppImage"));
        // deb, rpm, AUR, tarball: the binary on the PATH.
        assert_eq!(xwayland_command(None, None, true).as_deref(), Some("WAYLAND_DISPLAY= photosplice"));
        assert_eq!(xwayland_command(Some(""), Some(""), true).as_deref(), Some("WAYLAND_DISPLAY= photosplice"));
        // No X server (XWayland disabled): nothing to suggest.
        assert_eq!(xwayland_command(None, Some("/opt/p.AppImage"), false), None);
    }

    /// Tests every native save dialog and records the suggested file name. Each name's file type must come first
    /// in the save panel, or the panel appends the first type's extension (`photo.webp.psd`, `photo.gif.psd`).
    #[test]
    fn every_save_dialog_leads_with_its_own_extension() {
        let asked: Rc<RefCell<Vec<String>>> = Rc::default();
        let log = asked.clone();
        // Record each save dialog's suggested name and cancel it, as the user would.
        let services = Services {
            file_dialog: Some(Box::new(move |request, _parent, reply| {
                if let FileDialogRequest::Save { suggested } = request {
                    log.borrow_mut().push(suggested);
                }
                reply.send(None);
            })),
            ..Default::default()
        };
        let mut app = PhotospliceApp::new(Session::new(), services);
        let ctx = egui::Context::default();
        // Dialogs are shown on the next frame: poll once so each is answered before the next asks.
        let invoke = |app: &mut PhotospliceApp, id: &str| {
            let r = photosplice_ui_egui::menus::invoke(app, &ctx, id, json!({}));
            app.poll_file_dialog(&ctx, None);
            r
        };
        let dialog = |app: &mut PhotospliceApp, id: &str, fields: Value| {
            let d = invoke(app, id).unwrap()["dialog"].as_u64().unwrap();
            for (k, v) in fields.as_object().unwrap() {
                app.ui.dialog_mut(d).unwrap().fields.insert(k.clone(), v.clone());
            }
            let _ = photosplice_ui_egui::dialogs::confirm(app, d);
            app.poll_file_dialog(&ctx, None);
        };
        new_doc(&mut app, "#ff0000");
        // Save a Copy. (PhotoSplice's Save As is the Save for Web (Legacy) screen, which asks
        // for its file name through the dialog below.)
        let _ = invoke(&mut app, "file.saveACopy");
        // Export As, Quick Export, Save for Web (with and without slices).
        for format in ["png", "jpg", "webp", "tif", "tga"] {
            dialog(&mut app, "file.export.exportAs", json!({"format": format}));
        }
        for format in ["png", "jpg", "gif", "webp"] {
            app.run("prefs.set", json!({"path": "export.quickExportFormat", "value": format})).unwrap();
            let _ = invoke(&mut app, "file.export.quickExportAsPng");
        }
        for format in ["gif", "png8", "png24", "jpeg", "wbmp"] {
            dialog(&mut app, "file.export.saveForWebLegacy", json!({"format": format}));
        }
        app.run("slice.new", json!({"x": 0, "y": 0, "width": 2, "height": 2})).unwrap();
        dialog(&mut app, "file.export.saveForWebLegacy", json!({"html": true}));
        // Measurement Log, Export/Import Presets.
        app.run("image.analysis.recordMeasurements", json!({})).unwrap();
        let _ = invoke(&mut app, "measurementLog.export");
        dialog(&mut app, "edit.presets.exportImportPresets", json!({"action": "export"}));
        let asked = asked.borrow();
        let exts: Vec<&str> = asked.iter().filter_map(|s| s.rsplit_once('.').map(|(_, e)| e)).collect();
        // The extension each save above should suggest, in order.
        let want = [
            "psd",                       // Save a Copy
            "png jpg webp tif tga",      // Export As
            "png jpg gif webp",          // Quick Export
            "gif png png jpg wbmp html", // Save for Web: gif png8 png24 jpeg wbmp, then with slices
            "csv pcpresets",             // Measurement Log, Export Presets
        ]
        .join(" ");
        assert_eq!(exts, want.split(' ').collect::<Vec<_>>());
        for (name, ext) in asked.iter().zip(exts) {
            // The first type's first extension is the one Windows adds to the bare name.
            assert_eq!(save_filters(name)[0].1.first().map(String::as_str), Some(ext), "{name}: {:?}", save_filters(name)[0]);
        }
        assert_eq!(save_filters("Untitled")[0].0, "Photoshop", "no extension keeps the default");
    }

    /// Every flat format Save As offers is one PhotoSplice writes, and ICO is among them (#4357).
    #[test]
    fn save_as_offers_only_writable_formats_including_ico() {
        // Layered formats, and PDF, are written by `photosplice-io`, not a codec.
        let layered = ["psd", "psb", "pcraft", "ora", "pdf"];
        for (name, exts) in SAVE_FILTERS {
            for ext in exts.iter().filter(|e| !layered.contains(e)) {
                let format = photosplice_codecs::from_extension(ext).unwrap_or_else(|| panic!("{name}: .{ext} is no known format"));
                assert!(format.caps().write, "{name}: .{ext} can't be written");
            }
        }
        for ext in ["ico", "gif", "bmp"] {
            assert!(save_filters("Untitled-1.psd").iter().any(|(_, exts)| exts.iter().any(|e| e == ext)), ".{ext} is offered");
        }
        assert_eq!(save_filters("icon.ico")[0].0, "ICO", "an .ico name leads with its own type");
        // What Save As writes there: an icon up to 256×256, and a clear error past that.
        let doc = |side: u32| {
            let mut s = Session::new();
            s.execute("file.new", json!({"width": side, "height": side})).unwrap();
            s.active().unwrap().doc.clone()
        };
        let opts = export_options(&photosplice_ui_egui::ExportSettings::default());
        let icon = photosplice_io::export(&doc(256), "icon.ico", &opts).unwrap();
        assert_eq!(icon.bytes[..4], [0, 0, 1, 0], "an ICO header");
        let err = photosplice_io::export(&doc(300), "icon.ico", &opts).map(|_| ()).unwrap_err().to_string();
        assert!(err.contains("256"), "{err}");
    }

    /// #4394: with File name extensions shown, Windows kept `image.psd` when the type was changed to
    /// PhotoSplice. Given the bare `image`, it adds the extension of whichever type is picked
    /// (checked by hand with rfd 0.17.2 on Windows 11), defaulting to the first type's.
    #[test]
    fn the_windows_save_dialog_gets_a_bare_name_so_the_type_picks_the_extension() {
        for (suggested, name, first) in [
            ("/pics/image.psd", "image", "psd"),
            ("/pics/image.psb", "image", "psb"),
            ("/pics/image.pcraft", "image", "pcraft"),
            ("/pics/sky.fit", "sky", "fit"),
            ("cat.jpeg", "cat", "jpeg"),
            ("swatches.aco", "swatches", "aco"),
        ] {
            assert_eq!(dialog_file_name(suggested, true).as_deref(), Some(name), "{suggested}");
            assert_eq!(save_filters(suggested)[0].1[0], first, "{suggested}");
            // Elsewhere the dialogs keep the whole name (see `dialog_file_name`).
            let whole = Path::new(suggested).file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(dialog_file_name(suggested, false), Some(whole), "{suggested}");
        }
        // Kept whole: a second dot (Windows could keep `scan.jpg`), the user's upper case, no
        // extension, and a dotfile.
        for name in ["scan.jpg.psd", "my.photo.psd", "transparent.PNG", "Untitled", ".psd"] {
            assert_eq!(dialog_file_name(name, true).as_deref(), Some(name));
        }
        assert_eq!(dialog_file_name("", true), None);
    }

    #[test]
    fn block_on_waits_for_a_wake_from_another_thread() {
        let (tx, rx) = std::sync::mpsc::channel::<std::task::Waker>();
        let mut woken = false;
        let ready = std::future::poll_fn(move |cx| {
            if woken {
                return std::task::Poll::Ready(7);
            }
            woken = true;
            let _ = tx.send(cx.waker().clone());
            std::task::Poll::Pending
        });
        let waker = std::thread::spawn(move || rx.recv().unwrap().wake());
        assert_eq!(block_on(ready), 7);
        waker.join().unwrap();
    }

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
    const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

    /// One app launch with crash recovery in `dir` (dropping it is the crash: background writes
    /// already queued finish, nothing else runs).
    fn launch(dir: &Path) -> PhotospliceApp {
        let mut app = PhotospliceApp::new(Session::new(), recovery_services(Some(dir.to_path_buf())));
        let ctx = egui::Context::default();
        prefs_ui::tick(&mut app, &ctx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.session.has_jobs() && std::time::Instant::now() < deadline {
            photosplice_ui_egui::jobs_ui::tick(&mut app, &ctx);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!app.session.has_jobs(), "recovery finished");
        app
    }

    /// The first pixel of each open document, and whether it's unsaved.
    fn open_docs(app: &PhotospliceApp) -> Vec<([f32; 4], bool)> {
        app.session.documents().iter().map(|d| (photosplice_compose::flatten(&d.doc).px.first().copied().unwrap_or_default(), d.is_dirty())).collect()
    }

    fn index_of(app: &PhotospliceApp, color: [f32; 4]) -> usize {
        open_docs(app).iter().position(|(c, _)| *c == color).unwrap()
    }

    fn new_doc(app: &mut PhotospliceApp, color: &str) {
        app.run("file.new", json!({"width": 4, "height": 4})).unwrap();
        app.run("edit.fill", json!({"color": color})).unwrap();
    }

    fn autosave(app: &mut PhotospliceApp, ctx: &egui::Context) {
        prefs_ui::autosave_now(app);
        prefs_ui::tick(app, ctx);
    }

    #[test]
    fn recovered_documents_survive_a_second_crash_until_saved_or_closed() {
        let dir = std::env::temp_dir().join(format!("photosplice-recovery-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = egui::Context::default();
        // Launch 1: two unsaved documents are autosaved, then the app crashes.
        {
            let mut app = launch(&dir);
            new_doc(&mut app, "#ff0000");
            new_doc(&mut app, "#0000ff");
            autosave(&mut app, &ctx);
        }
        assert_eq!(list_recovery(&dir).len(), 2);
        // Launch 2 recovers both and crashes again before its first autosave (#444).
        {
            let mut app = launch(&dir);
            let mut docs = open_docs(&app);
            docs.sort_by(|a, b| a.0[0].total_cmp(&b.0[0]));
            assert_eq!(docs, [(BLUE, true), (RED, true)], "recovered and still unsaved");
            prefs_ui::tick(&mut app, &ctx);
        }
        assert_eq!(list_recovery(&dir).len(), 2, "recovering keeps the entries on disk");
        // Launch 3 has both again; autosaving unchanged documents adds nothing, a new document
        // (whose id may repeat one from an earlier launch) gets an entry of its own.
        {
            let mut app = launch(&dir);
            assert_eq!(app.session.documents().len(), 2);
            autosave(&mut app, &ctx);
            new_doc(&mut app, "#00ff00");
            autosave(&mut app, &ctx);
        }
        assert_eq!(list_recovery(&dir).len(), 3);
        // Launch 4: closing and saving recovered documents removes their entries, no duplicates.
        let mut app = launch(&dir);
        assert_eq!(app.session.documents().len(), 3);
        let red = index_of(&app, RED);
        app.run("file.close", json!({"document": red})).unwrap();
        // Saving marks the revision saved (the file write itself is the save service's job).
        assert!(app.session.set_active(index_of(&app, BLUE)));
        let st = app.session.active_mut().unwrap();
        st.saved_revision = st.revision;
        prefs_ui::tick(&mut app, &ctx);
        let left = list_recovery(&dir);
        assert_eq!(left.len(), 1);
        assert_eq!(photosplice_compose::flatten(&photosplice_format::recover(&left[0]).unwrap()).px.first().copied(), Some(GREEN));
        let green = index_of(&app, GREEN);
        app.run("file.close", json!({"document": green})).unwrap();
        prefs_ui::tick(&mut app, &ctx);
        assert!(list_recovery(&dir).is_empty());
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_background_autosave_is_reported_and_retried_without_another_edit() {
        let dir = temp("autosave-retry");
        let recovery = dir.join("Recovery");
        // A regular file prevents creation of the recovery bundle directory.
        std::fs::write(&recovery, b"blocked").unwrap();
        let ctx = egui::Context::default();
        let mut app = launch(&recovery);
        new_doc(&mut app, "#ff0000");
        let revision = app.session.active().unwrap().revision;
        assert_ne!(revision, 0, "a sidecar-less bundle lists as revision 0");
        autosave(&mut app, &ctx);

        let mut failed = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            prefs_ui::tick(&mut app, &ctx);
            if app.ui.status.starts_with("Autosave failed:") {
                failed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(failed, "background write failures must reach the UI");
        assert!(app.ui.status_error);
        assert!(list_recovery(&recovery).is_empty());

        // No edit occurs: retrying this very same revision must still work.
        std::fs::remove_file(&recovery).unwrap();
        std::fs::create_dir(&recovery).unwrap();
        autosave(&mut app, &ctx);
        // The worker commits the bundle's manifest before it publishes the sidecar, and
        // `list_recovery` lists a committed sidecar-less bundle (revision 0) on its own, so
        // the first non-empty listing can come from the middle of the write. Wait for the write
        // to finish: the entry then carries the sidecar's revision.
        let mut recovered = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            prefs_ui::tick(&mut app, &ctx);
            recovered = list_recovery(&recovery);
            if recovered.iter().any(|e| e.info.revision == revision) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(recovered.len(), 1, "unchanged revision should retry after failure");
        assert_eq!(recovered[0].info.revision, revision);
        let restored = photosplice_format::recover(&recovered[0]).unwrap();
        assert_eq!(photosplice_compose::flatten(&restored).px.first().copied(), Some(RED));
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("photosplice-services-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn red_png(w: u32, h: u32) -> Vec<u8> {
        let img = Image::from_u8(w, h, ChannelLayout::Rgba, [255u8, 0, 0, 255].repeat((w * h) as usize)).unwrap();
        photosplice_codecs::encode(&img, photosplice_codecs::Format::Png, &EncodeOptions::default()).unwrap()
    }

    /// A 1×1 uncompressed 32-bit TGA (TGA has no magic number).
    fn tga_1x1() -> Vec<u8> {
        let mut b = vec![0u8, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 32, 8];
        b.extend_from_slice(&[0, 0, 255, 255]); // BGRA red
        b
    }

    #[test]
    fn copied_image_file_pastes_its_pixels() {
        let dir = temp("png");
        let png = dir.join("red.png");
        std::fs::write(&png, red_png(3, 2)).unwrap();
        let (w, h, px) = image_from_files(&[png]).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(px, [255u8, 0, 0, 255].repeat(6));
    }

    #[test]
    fn crlf_from_a_uri_list_is_ignored() {
        // arboard splits text/uri-list on LF, so GNOME Files' CRLF list leaves a '\r' behind.
        let dir = temp("crlf");
        let png = dir.join("red image.png");
        std::fs::write(&png, red_png(2, 2)).unwrap();
        let with_cr = PathBuf::from(format!("{}\r", png.display()));
        assert_eq!(image_from_files(&[with_cr]).map(|(w, h, _)| (w, h)), Some((2, 2)));
    }

    #[test]
    fn non_images_are_skipped_for_the_first_image() {
        let dir = temp("mixed");
        let (txt, png) = (dir.join("notes.txt"), dir.join("red.png"));
        std::fs::write(&txt, b"not an image").unwrap();
        std::fs::write(&png, red_png(4, 1)).unwrap();
        let missing = dir.join("gone.png");
        assert_eq!(image_from_files(&[missing.clone(), txt.clone(), dir.clone(), png]).map(|(w, h, _)| (w, h)), Some((4, 1)));
        assert!(image_from_files(&[missing, txt, dir]).is_none());
        assert!(image_from_files(&[]).is_none());
    }

    #[test]
    fn a_damaged_image_is_skipped_not_a_panic() {
        let dir = temp("damaged");
        let bad = dir.join("cut.png");
        std::fs::write(&bad, &red_png(8, 8)[..20]).unwrap();
        assert!(image_from_files(&[bad]).is_none());
    }

    fn encoded(format: photosplice_codecs::Format, w: u32, h: u32) -> Vec<u8> {
        let img = Image::from_u8(w, h, ChannelLayout::Rgba, [255u8, 0, 0, 255].repeat((w * h) as usize)).unwrap();
        photosplice_codecs::encode(&img, format, &EncodeOptions::default()).unwrap()
    }

    #[test]
    fn non_png_clipboard_images_paste() {
        // #2546: a screenshot tool that offers only image/bmp or image/jpeg pasted nothing.
        let bmp = encoded(photosplice_codecs::Format::Bmp, 3, 2);
        let offers = vec!["text/plain".to_string(), "image/bmp".to_string()];
        let mut asked = Vec::new();
        let got = image_from_offers(&offers, |mime| {
            asked.push(mime.to_string());
            Some(bmp.clone())
        });
        assert_eq!(got, Some((3, 2, [255u8, 0, 0, 255].repeat(6))));
        assert_eq!(asked, ["image/bmp"], "text offers are never fetched");

        let jpeg = encoded(photosplice_codecs::Format::Jpeg, 16, 8);
        let offers = vec!["image/jpeg".to_string()];
        assert_eq!(image_from_offers(&offers, |_| Some(jpeg.clone())).map(|(w, h, _)| (w, h)), Some((16, 8)));
    }

    #[test]
    fn undecodable_clipboard_offers_fall_through_to_the_next() {
        let bmp = encoded(photosplice_codecs::Format::Bmp, 2, 2);
        let cut = encoded(photosplice_codecs::Format::Png, 8, 8)[..20].to_vec();
        let offers: Vec<String> = ["image/webp", "image/png", "image/x-tga", "image/bmp"].map(String::from).to_vec();
        let got = image_from_offers(&offers, |mime| match mime {
            "image/webp" => None,
            "image/png" => Some(cut.clone()),
            "image/x-tga" => Some(b"not an image".to_vec()),
            _ => Some(bmp.clone()),
        });
        assert_eq!(got.map(|(w, h, _)| (w, h)), Some((2, 2)));
        assert!(image_from_offers(&offers[..3], |_| Some(cut.clone())).is_none());
        assert!(image_from_offers(&[], |_| Some(bmp.clone())).is_none());
    }

    #[test]
    fn clipboard_tga_is_trusted_only_by_its_type() {
        assert_eq!(image_from_offers(&["image/x-tga".to_string()], |_| Some(tga_1x1())), Some((1, 1, vec![255, 0, 0, 255])));
        assert!(image_from_offers(&["image/x-unknown".to_string()], |_| Some(tga_1x1())).is_none());
    }

    #[test]
    fn tga_is_trusted_only_with_its_extension() {
        let dir = temp("tga");
        let (tga, bin) = (dir.join("red.tga"), dir.join("red.bin"));
        std::fs::write(&tga, tga_1x1()).unwrap();
        std::fs::write(&bin, tga_1x1()).unwrap();
        assert_eq!(image_from_files(&[tga]), Some((1, 1, vec![255, 0, 0, 255])));
        assert!(image_from_files(&[bin]).is_none());
    }
}
