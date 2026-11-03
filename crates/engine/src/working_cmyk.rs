//! Edit › Color Settings › Working CMYK: the profile CMYK documents without one of their own are
//! separated with, and CMYK colour values are read in.
//!
//! PhotoSplice's default follows Reference's Europe General Purpose 3 settings: Coated FOGRA39
//! (ISO 12647-2:2004) with relative colorimetric and black point compensation (Reference's North
//! America default, U.S. Web Coated (SWOP) v2, is one of the installed choices). That profile may
//! not be shipped, so `auto` uses it when Reference's installer or the system put it on disk
//! (`photosplice_cms::installed`), and the built-in coated CMYK otherwise (always on the web).
//! A configured profile (`coated-cmyk` or an `.icc` path) is used as it is; one that no longer
//! reads falls back to `auto`.
//!
//! The working CMYK is process-wide (`photosplice_color::convert::set_working_cmyk`): the
//! compositor, the GPU upload, composite exports and the CMS conversions read it.
//! [`Session::apply_working_cmyk`] sets it from Color Settings; the application calls it once at
//! startup, and Color Settings and the preferences call it when the setting changes.

use std::sync::Arc;

use photosplice_cms::{Builtin, ColorSpace, Profile};
use photosplice_color::ColorMode;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// The setting that means Reference's default when it is installed.
pub const AUTO: &str = "auto";

/// What a Working CMYK setting resolves to.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub profile: Arc<Profile>,
    /// `installed` (`auto` found Coated FOGRA39), `builtin` or `configured`.
    pub source: &'static str,
    /// The file it was read from, when it was.
    pub path: Option<String>,
    /// The configured profile did not resolve to a CMYK profile, so `auto` stands in.
    pub fallback: bool,
}

fn builtin() -> Resolved {
    Resolved { profile: Arc::new(Builtin::CoatedCmyk.profile().clone()), source: "builtin", path: None, fallback: false }
}

fn auto() -> Resolved {
    match photosplice_cms::installed::fogra39() {
        Some(f) => Resolved { profile: f.profile, source: "installed", path: Some(f.path.to_string_lossy().into_owned()), fallback: false },
        None => builtin(),
    }
}

/// Resolves a Working CMYK setting (`auto`, a built-in id or an `.icc` path). Reads the disk for
/// a path, and once per process for `auto`.
pub fn resolve(spec: &str) -> Resolved {
    match spec {
        "" | AUTO | "working" | "default" | "working-cmyk" | "workingCmyk" => auto(),
        other => match crate::color_cmds::resolve_profile(other, None, Some(ColorMode::Cmyk)) {
            Ok(p) if p.color_space == ColorSpace::Cmyk => {
                if p.content_hash() == Builtin::CoatedCmyk.profile().content_hash() {
                    builtin()
                } else {
                    let path = Builtin::from_id(other).is_none().then(|| other.to_string());
                    Resolved { profile: p, source: "configured", path, fallback: false }
                }
            }
            _ => Resolved { fallback: true, ..auto() },
        },
    }
}

impl Session {
    /// Makes Color Settings › Working CMYK the process-wide working CMYK and redraws the CMYK
    /// documents when it changed. Returns what it resolved to.
    pub fn apply_working_cmyk(&mut self) -> Resolved {
        let r = resolve(&self.color.settings.working_cmyk);
        if photosplice_color::convert::set_working_cmyk(Some(r.profile.clone())) {
            for st in self.docs.iter_mut().filter(|st| st.doc.mode == ColorMode::Cmyk) {
                // Not an edit: a clean document stays clean.
                let clean = st.saved_revision == st.revision;
                st.revision += 1;
                if clean {
                    st.saved_revision = st.revision;
                }
                st.last_damage = None;
            }
        }
        r
    }
}

/// The CMYK space CMYK colour values are read in for the active document (the Color Picker's
/// CMYK fields): a CMYK document's own profile, else the working CMYK. Enter it with
/// `photosplice_color::convert::with_cmyk_space`.
pub fn values_space(s: &Session) -> Option<Arc<photosplice_color::convert::CmykSpace>> {
    let icc = s.active().and_then(|d| d.doc.icc_profile.as_ref().filter(|_| d.doc.mode == ColorMode::Cmyk));
    photosplice_color::convert::CmykSpace::for_profile(icc)
}

fn report(s: &Session) -> Value {
    let setting = s.color.settings.working_cmyk.clone();
    let r = resolve(&setting);
    let mut choices = vec![
        json!({"id": AUTO, "description": format!("Automatic ({} if installed)", photosplice_cms::installed::FOGRA39.description)}),
        json!({"id": Builtin::CoatedCmyk.id(), "description": Builtin::CoatedCmyk.description()}),
    ];
    choices.extend(
        crate::installed_profiles::installed()
            .into_iter()
            .filter(|i| i.color_space == ColorSpace::Cmyk && i.is_destination())
            .map(|i| json!({"id": i.path, "description": i.description})),
    );
    let in_use = photosplice_color::convert::working_cmyk_profile();
    json!({
        "setting": setting,
        "profile": r.profile.description,
        "source": r.source,
        "path": r.path,
        "fallback": r.fallback,
        // What untagged CMYK documents render with right now (the setting once applied).
        "inUse": in_use.description,
        "choices": choices,
    })
}

/// `edit.workingCmyk`: read Working CMYK, or set it (`profile`).
fn working_cmyk(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "edit.workingCmyk";
    let bad = |msg: String| EngineError::BadParams { cmd: cmd.into(), msg };
    match p.get("profile") {
        None | Some(Value::Null) => {}
        Some(Value::String(spec)) => {
            if spec.trim().is_empty() {
                return Err(bad("`profile` is empty (auto|coated-cmyk or a path to a CMYK .icc file)".into()));
            }
            if spec != AUTO {
                let prof = crate::color_cmds::resolve_profile(spec, None, Some(ColorMode::Cmyk)).map_err(|e| bad(e.to_string()))?;
                if prof.color_space != ColorSpace::Cmyk {
                    return Err(bad(format!("`{spec}` is a {:?} profile, not CMYK", prof.color_space)));
                }
            }
            if *spec != s.color.settings.working_cmyk {
                s.color.settings.working_cmyk = spec.clone();
                s.color.settings.working_cmyk_rev = 1;
                // Persisted with the preferences.
                s.prefs.edit(|_| ());
            }
            s.apply_working_cmyk();
        }
        Some(v) => return Err(bad(format!("`profile` must be a string, not {v}"))),
    }
    Ok(report(s))
}

/// `edit.workingCmyk`.
pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "edit.workingCmyk",
        label: "Working CMYK",
        menu: &[],
        shortcut: None,
        params: r##"{"profile":"auto|coated-cmyk" (or a path to a CMYK .icc file; omit to read)} → {setting, profile, source: installed|builtin|configured, path, fallback, inUse, choices: [{id, description}]} (auto = Coated FOGRA39 (ISO 12647-2:2004) when installed, else the built-in coated CMYK; untagged CMYK documents and CMYK colours use it)"##,
        enabled: |_| Ok(()),
        run: working_cmyk,
        journal: true,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests that change the process-wide working CMYK live in `tests/working_cmyk.rs`: here they
    // would race every unit test that renders a CMYK document.

    #[test]
    fn resolution_order_and_fallback() {
        assert_eq!(resolve("coated-cmyk").source, "builtin");
        let a = resolve(AUTO);
        assert!(matches!(a.source, "installed" | "builtin"));
        assert_eq!(a.path.is_some(), a.source == "installed");
        // A configured file wins over `auto`.
        let dir = std::env::temp_dir().join(format!("photosplice-working-cmyk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("press.icc");
        let p = photosplice_cms::synth::cmyk_profile(&photosplice_cms::synth::CmykParams {
            description: "Press CMYK".into(),
            grid_a2b: 3,
            grid_b2a: 3,
            ..Default::default()
        });
        std::fs::write(&file, p.to_bytes().as_slice()).unwrap();
        let spec = file.to_string_lossy().into_owned();
        let r = resolve(&spec);
        assert_eq!((r.source, r.path.as_deref(), r.profile.description.as_str(), r.fallback), ("configured", Some(spec.as_str()), "Press CMYK", false));
        // Gone, or not CMYK: `auto` stands in.
        std::fs::write(&file, Builtin::Srgb.profile().to_bytes().as_slice()).unwrap();
        let r = resolve(&spec);
        assert!(r.fallback && r.source == a.source);
        std::fs::remove_file(&file).unwrap();
        assert!(resolve(&spec).fallback);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn command_reads_and_rejects_bad_params() {
        let mut s = Session::new();
        let r = s.execute("edit.workingCmyk", json!({})).unwrap();
        assert_eq!(r["setting"], AUTO);
        assert!(r["choices"].as_array().unwrap().iter().any(|c| c["id"] == "coated-cmyk"));
        for bad in [
            json!({"profile": ""}),
            json!({"profile": 3}),
            json!({"profile": []}),
            json!({"profile": "srgb"}),
            json!({"profile": "/definitely/not/a/profile.icc"}),
            json!({"profile": "no-such-profile"}),
        ] {
            assert!(s.execute("edit.workingCmyk", bad.clone()).is_err(), "{bad}");
        }
        assert_eq!(s.color.settings.working_cmyk, AUTO, "a refused profile changes nothing");
    }

    #[test]
    fn old_preferences_load_as_auto() {
        let mut c = crate::color_cmds::ColorSettings { working_cmyk: "coated-cmyk".into(), working_cmyk_rev: 0, ..Default::default() };
        c.migrate();
        assert_eq!((c.working_cmyk.as_str(), c.working_cmyk_rev), (AUTO, crate::color_cmds::SETTINGS_REV));
        // A choice made since is kept.
        let mut c = crate::color_cmds::ColorSettings { working_cmyk: "coated-cmyk".into(), ..Default::default() };
        c.migrate();
        assert_eq!(c.working_cmyk, "coated-cmyk");
        // Saved files without the field are the old ones.
        let old: crate::color_cmds::ColorSettings = serde_json::from_value(json!({"workingCmyk": "coated-cmyk"})).unwrap();
        assert_eq!(old.working_cmyk_rev, 0);
        let new: crate::color_cmds::ColorSettings = serde_json::from_value(serde_json::to_value(crate::color_cmds::ColorSettings::default()).unwrap()).unwrap();
        assert_eq!(new.working_cmyk_rev, crate::color_cmds::SETTINGS_REV);
    }

    /// Europe General Purpose 3 is the default: Dot Gain 15 % gray, no profile warnings. Settings
    /// saved with the old North America defaults move to it; other choices stay.
    #[test]
    fn europe_defaults_and_their_migration() {
        let d = crate::color_cmds::ColorSettings::default();
        assert_eq!((d.working_gray.as_str(), d.ask_on_mismatch, d.ask_on_paste, d.ask_on_missing), ("dot-gain-15-compat", false, false, false));
        let mut old = crate::color_cmds::ColorSettings { working_gray: "dot-gain-20-compat".into(), ask_on_mismatch: true, ask_on_paste: true, working_cmyk_rev: 1, ..d.clone() };
        old.migrate();
        assert_eq!((old.working_gray.as_str(), old.ask_on_mismatch, old.ask_on_paste), ("dot-gain-15-compat", false, false));
        let mut chosen = crate::color_cmds::ColorSettings { working_gray: "sgray".into(), working_cmyk_rev: 1, ..d.clone() };
        chosen.migrate();
        assert_eq!(chosen.working_gray, "sgray");
        // Up to date: left alone.
        let mut now = crate::color_cmds::ColorSettings { working_gray: "dot-gain-20-compat".into(), ask_on_mismatch: true, ..d };
        now.migrate();
        assert_eq!((now.working_gray.as_str(), now.ask_on_mismatch), ("dot-gain-20-compat", true));
    }
}
