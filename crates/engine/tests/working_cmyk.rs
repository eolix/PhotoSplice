//! Edit › Color Settings › Working CMYK drives the process-wide working CMYK
//! (`photosplice_color::convert::set_working_cmyk`) that untagged CMYK documents render, export
//! and convert through.
//!
//! In its own integration-test binary on purpose (like `text_gamma.rs`): changing the working
//! CMYK changes how every untagged CMYK document composites, which would race the engine's unit
//! tests. The tests here take [`LOCK`], so they never overlap each other either.

use std::sync::{Arc, Mutex, PoisonError};

use photosplice_cms::synth::{CmykParams, cmyk_profile};
use photosplice_cms::{Builtin, Intent, Transform};
use photosplice_color::convert::{working_cmyk, working_cmyk_profile};
use photosplice_color::{ColorMode, PixelFormat, SampleType};
use photosplice_doc::{Document, Layer, LayerContent, Size};
use photosplice_engine::Session;
use photosplice_raster::Surface;
use serde_json::json;

static LOCK: Mutex<()> = Mutex::new(());

/// An untagged 8-bit CMYK document of varied inks.
fn untagged_cmyk() -> Document {
    let mut d = Document::new("cmyk", Size::new(8, 4), ColorMode::Cmyk, SampleType::U8);
    let mut bg = Surface::new(PixelFormat::new(ColorMode::Cmyk, SampleType::U8, false));
    for y in 0..4 {
        for x in 0..8 {
            bg.write_pixel(x, y, &[x as f32 / 7.0, y as f32 / 3.0, 0.5 - x as f32 / 16.0, if x == 7 { 1.0 } else { 0.1 * y as f32 }]);
        }
    }
    d.layers.push(Layer::new("Background", LayerContent::Raster(bg)));
    d
}

fn flat(s: &Session) -> Vec<[f32; 4]> {
    photosplice_compose::flatten(&s.active().unwrap().doc).px
}

fn max_diff(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter().zip(b).flat_map(|(p, q)| (0..4).map(move |c| (p[c] - q[c]).abs())).fold(0.0, f32::max)
}

fn temp_profile(tag: &str, description: &str) -> String {
    let dir = std::env::temp_dir().join(format!("photosplice-engine-working-cmyk-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = cmyk_profile(&CmykParams { description: description.into(), tvi: [0.26, 0.26, 0.26, 0.3], grid_a2b: 5, grid_b2a: 9, ..Default::default() });
    let file = dir.join("press.icc");
    std::fs::write(&file, p.to_bytes().as_slice()).unwrap();
    file.to_string_lossy().into_owned()
}

#[test]
fn working_cmyk_setting_separates_untagged_documents() {
    let _guard = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut s = Session::new();
    s.execute("edit.workingCmyk", json!({"profile": "coated-cmyk"})).unwrap();
    assert!(working_cmyk().is_none(), "the built-in profile is the default path");
    s.add_document(untagged_cmyk(), None);
    let builtin = flat(&s);
    let st = s.active().unwrap();
    let (rev, clean) = (st.revision, st.saved_revision == st.revision);

    // A configured profile file: untagged CMYK renders through it, and stays untagged.
    let path = temp_profile("configured", "Working Test CMYK");
    let r = s.execute("edit.workingCmyk", json!({"profile": path})).unwrap();
    assert_eq!((r["source"].as_str(), r["path"].as_str(), r["inUse"].as_str()), (Some("configured"), Some(path.as_str()), Some("Working Test CMYK")));
    assert_eq!(s.color.settings.working_cmyk, path);
    let st = s.active().unwrap();
    assert!(st.revision > rev, "CMYK documents redraw");
    assert_eq!(st.saved_revision == st.revision, clean, "and stay as clean as they were");
    assert!(st.doc.icc_profile.is_none());
    let working = flat(&s);
    assert!(max_diff(&builtin, &working) > 0.02);
    let mut tagged = untagged_cmyk();
    tagged.icc_profile = Some(working_cmyk_profile().to_bytes());
    assert!(max_diff(&working, &photosplice_compose::flatten(&tagged).px) < 1e-6, "the same as embedding it");
    let r = s.execute("edit.colorSettings", json!({})).unwrap();
    assert_eq!(r["working"]["cmyk"], "Working Test CMYK");

    // Saving does not embed the working profile into a document that had none.
    let out = photosplice_io::export(&s.active().unwrap().doc, "x.psd", &photosplice_io::ExportOptions::default()).unwrap();
    assert!(photosplice_io::import("x.psd", &out.bytes).unwrap().document.icc_profile.is_none());

    // Image › Mode › CMYK Color converts through it and tags the document, as Reference does.
    let mut rgb = Document::new("rgb", Size::new(2, 2), ColorMode::Rgb, SampleType::U8);
    let mut px = Surface::new(PixelFormat::new(ColorMode::Rgb, SampleType::U8, false));
    px.fill_rect(photosplice_geom::Rect::new(0, 0, 2, 2), &[0.2, 0.5, 0.8]);
    rgb.layers.push(Layer::new("Background", LayerContent::Raster(px)));
    s.add_document(rgb, None);
    s.execute("image.mode.cmyk", json!({})).unwrap();
    let d = &s.active().unwrap().doc;
    assert_eq!(d.mode, ColorMode::Cmyk);
    assert_eq!(photosplice_cms::Profile::parse(d.icc_profile.as_ref().unwrap()).unwrap().description, "Working Test CMYK");
    s.set_active(0);

    // A path that no longer reads is refused, and nothing changes.
    assert!(s.execute("edit.colorSettings", json!({"workingCmyk": "/definitely/missing.icc"})).is_err());
    assert!(s.execute("edit.workingCmyk", json!({"profile": "/definitely/missing.icc"})).is_err());
    assert_eq!(working_cmyk_profile().description, "Working Test CMYK");

    // Color Settings, the preferences and a preferences file move it too.
    s.execute("edit.colorSettings", json!({"workingCmyk": "coated-cmyk"})).unwrap();
    assert!(working_cmyk().is_none());
    assert!(max_diff(&builtin, &flat(&s)) < 1e-6);
    s.execute("prefs.set", json!({"path": "colorSettings.workingCmyk", "value": path})).unwrap();
    assert_eq!(working_cmyk_profile().description, "Working Test CMYK");
    s.execute("prefs.reset", json!({"path": "colorSettings.workingCmyk"})).unwrap();
    assert_eq!(s.color.settings.working_cmyk, "auto");
    let auto = photosplice_cms::installed::swop_v2();
    assert_eq!(working_cmyk().map(|w| w.id), auto.as_ref().map(|f| f.profile.content_hash()), "auto: the installed SWOP v2, else the built-in");
    // Preferences saved before `auto` existed held the built-in profile as the default.
    s.execute("edit.workingCmyk", json!({"profile": "coated-cmyk"})).unwrap();
    let mut old: serde_json::Value = serde_json::from_str(&s.prefs_to_json()).unwrap();
    old["colorSettings"].as_object_mut().unwrap().remove("workingCmykRev");
    let mut t = Session::new();
    t.load_prefs_json(&old.to_string()).unwrap();
    assert_eq!(t.color.settings.working_cmyk, "auto");
    // But the built-in profile chosen since loads as it is.
    let mut t = Session::new();
    t.load_prefs_json(&s.prefs_to_json()).unwrap();
    assert_eq!(t.color.settings.working_cmyk, "coated-cmyk");
    assert!(working_cmyk().is_none());
    let _ = std::fs::remove_file(&path);
}

/// With Reference's U.S. Web Coated (SWOP) v2 installed (not on CI), `auto` separates untagged
/// CMYK through it, relative colorimetric with black point compensation.
#[test]
fn auto_uses_the_installed_swop_profile() {
    let _guard = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(swop) = photosplice_cms::installed::swop_v2() else {
        eprintln!("U.S. Web Coated (SWOP) v2 is not installed: skipped");
        return;
    };
    let mut s = Session::new();
    let r = s.execute("edit.workingCmyk", json!({"profile": "auto"})).unwrap();
    assert_eq!((r["source"].as_str(), r["inUse"].as_str()), (Some("installed"), Some(photosplice_cms::installed::SWOP_V2_DESCRIPTION)));
    assert_eq!(r["path"].as_str(), Some(swop.path.to_string_lossy().as_ref()));
    s.add_document(untagged_cmyk(), None);
    let ours = flat(&s);
    let t = Transform::new(&swop.profile, Builtin::Srgb.profile(), Intent::RelativeColorimetric, true).unwrap();
    let doc = Arc::clone(&s.active().unwrap().doc);
    let bg = doc.layers[0].surface().unwrap();
    for (i, p) in ours.iter().enumerate() {
        let ink = bg.pixel((i % 8) as i32, (i / 8) as i32);
        let mut want = [0.0f32; 3];
        t.eval(&ink[..4], &mut want);
        for c in 0..3 {
            assert!((p[c] - want[c].clamp(0.0, 1.0)).abs() < 2.0 / 255.0, "pixel {i}: {p:?} vs SWOP {want:?}");
        }
    }
    // 100 % cyan is SWOP's cyan, not the built-in profile's.
    let mut builtin = [0.0f32; 3];
    Transform::new(Builtin::CoatedCmyk.profile(), Builtin::Srgb.profile(), Intent::RelativeColorimetric, true)
        .unwrap()
        .eval(&[1.0, 0.0, 0.0, 0.0], &mut builtin);
    let mut cyan = [0.0f32; 3];
    t.eval(&[1.0, 0.0, 0.0, 0.0], &mut cyan);
    assert!(cyan.iter().zip(builtin).any(|(a, b)| (a - b).abs() > 0.01), "{cyan:?} vs {builtin:?}");
    s.execute("edit.workingCmyk", json!({"profile": "coated-cmyk"})).unwrap();
}
