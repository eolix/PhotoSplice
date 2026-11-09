//! Edit › Color Settings › Working Gray drives the process-wide working gray
//! (`photosplice_color::convert::set_working_gray`) that untagged Grayscale documents display and
//! convert in, as in Reference.
//!
//! In its own integration-test binary on purpose (like `working_cmyk.rs`): changing the working
//! gray changes how every untagged gray document shows, which would race the engine's unit tests.

use photosplice_cms::Builtin;
use photosplice_color::convert::working_gray_profile;
use photosplice_color::{Color, ColorMode, SampleType};
use photosplice_doc::{Document, Size};
use photosplice_engine::Session;
use photosplice_engine::color_cmds::{document_profile, working_profile};
use serde_json::json;

#[test]
fn working_gray_setting_shows_and_converts_untagged_gray() {
    let mut s = Session::new();
    // The default is Europe General Purpose 3's Dot Gain 15 %, in the settings and the process.
    assert_eq!(s.color.settings.working_gray, Builtin::DotGain15Compat.id());
    assert_eq!(working_gray_profile().content_hash(), Builtin::DotGain15Compat.profile().content_hash());
    s.add_document(Document::with_background("g", Size::new(4, 4), ColorMode::Grayscale, SampleType::U8, Color::gray(0.3)), None);
    let doc = s.active().unwrap().doc.clone();
    assert_eq!(document_profile(&doc).description, Builtin::DotGain15Compat.description());
    assert!(!s.color.canvas_display(&doc).unwrap().is_identity());
    let st = s.active().unwrap();
    let (rev, clean) = (st.revision, st.saved_revision == st.revision);

    // sGray: the untagged document now shows as sRGB gray, and redraws without becoming dirty.
    s.execute("edit.colorSettings", json!({"workingGray": "sgray"})).unwrap();
    assert_eq!(working_profile(ColorMode::Grayscale).description, Builtin::SGray.description());
    let st = s.active().unwrap();
    assert!(st.revision > rev, "gray documents redraw");
    assert_eq!(st.saved_revision == st.revision, clean, "and stay as clean as they were");
    assert!(st.doc.icc_profile.is_none(), "and stay untagged");
    assert!(s.color.canvas_display(&doc).unwrap().is_identity());

    // Image › Mode › Grayscale converts into it and tags the document.
    s.execute("file.new", json!({"width": 2, "height": 2, "background": "white"})).unwrap();
    s.execute("image.mode.grayscale", json!({})).unwrap();
    let tagged = photosplice_cms::Profile::parse(s.active().unwrap().doc.icc_profile.as_ref().unwrap()).unwrap();
    assert_eq!(tagged.description, Builtin::SGray.description());

    // The preferences move it too, and resetting brings back Dot Gain 15 %.
    s.execute("prefs.set", json!({"path": "colorSettings.workingGray", "value": "dot-gain-20-compat"})).unwrap();
    assert_eq!(working_gray_profile().description, Builtin::DotGain20Compat.description());
    s.execute("prefs.reset", json!({"path": "colorSettings.workingGray"})).unwrap();
    assert_eq!(working_gray_profile().description, Builtin::DotGain15Compat.description());
    let mut prefs: serde_json::Value = serde_json::from_str(&s.prefs_to_json()).unwrap();
    prefs["colorSettings"]["workingGray"] = json!("gray-gamma-2.2");
    let mut t = Session::new();
    t.load_prefs_json(&prefs.to_string()).unwrap();
    assert_eq!(working_gray_profile().description, Builtin::GrayGamma22.description());
    s.execute("prefs.reset", json!({"path": "colorSettings.workingGray"})).unwrap();
}
