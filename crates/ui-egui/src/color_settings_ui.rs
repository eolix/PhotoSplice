//! Edit › Color Settings (⇧⌘K), laid out as Reference's: Working Spaces and Color Management
//! Policies on the left; Conversion Options, Advanced Controls and the monitor profile on the
//! right. The fields are `edit.colorSettings`'s params (the dialog opens through
//! `filter_dialog::open`), so OK, ⌥ Cancel → Reset and `ui.dialog.set` work as for any command
//! dialog; only the drawing is this module's.

use photosplice_cms::{Builtin, ColorSpace, ProfileClass};
use photosplice_engine::installed_profiles::{InstalledProfile, installed};
use serde_json::{Map, Value, json};

use crate::PhotospliceApp;
use crate::theme::Tokens;
use crate::widgets;

pub const COMMAND: &str = "edit.colorSettings";

/// The dialog's width: the two columns side by side.
pub const WIDTH: f32 = 780.0;
const LEFT_W: f32 = 430.0;
const GAP: f32 = 14.0;

/// Blend Text Colors Using Gamma's value when it is ticked again after being off.
const TEXT_GAMMA: f64 = 1.45;

pub fn owns(f: &Map<String, Value>) -> bool {
    f.get("__command").and_then(Value::as_str) == Some(COMMAND)
}

fn s<'a>(f: &'a Map<String, Value>, key: &str) -> &'a str {
    f.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Built-in profiles by id, named by their descriptions.
fn builtins(ids: &[&'static str]) -> Vec<(String, &'static str)> {
    ids.iter().map(|id| (id.to_string(), Builtin::from_id(id).map_or(*id, Builtin::description))).collect()
}

/// A working space menu: the built-ins, then the installed profiles of `space` by description.
fn profile_menu<'a>(mut opts: Vec<(String, &'a str)>, profiles: &'a [InstalledProfile], space: ColorSpace, classes: &[ProfileClass]) -> Vec<(String, &'a str)> {
    opts.extend(
        profiles
            .iter()
            .filter(|p| p.color_space == space && p.is_destination() && classes.contains(&p.class))
            .map(|p| (p.path.clone(), p.description.as_str())),
    );
    opts
}

/// `Label:  [menu]` for the string field `key`. A value that isn't listed (a profile path an agent
/// typed) shows by its file name instead of a blank menu.
fn menu_row(ui: &mut egui::Ui, f: &mut Map<String, Value>, label: &str, label_w: f32, key: &str, opts: Vec<(String, &str)>, width: f32) {
    let mut cur = s(f, key).to_string();
    let stem = std::path::Path::new(&cur).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut opts: Vec<(String, &str)> = opts.iter().map(|(v, l)| (v.clone(), *l)).collect();
    if !opts.iter().any(|(v, _)| *v == cur) {
        opts.push((cur.clone(), stem.as_str()));
    }
    ui.horizontal(|ui| {
        widgets::form_label(ui, label, label_w);
        let w = width.min(ui.available_width()).max(80.0);
        if widgets::dropdown_fixed(ui, &format!("color-settings-{key}"), &mut cur, &opts, w) {
            f.insert(key.into(), json!(cur));
        }
    });
}

fn check(ui: &mut egui::Ui, f: &mut Map<String, Value>, key: &str, label: &str) {
    let mut on = f.get(key).and_then(Value::as_bool).unwrap_or(false);
    if widgets::checkbox(ui, &mut on, label).changed() {
        f.insert(key.into(), json!(on));
    }
}

/// A row of checkboxes after a label in the label column (or under it, `label` empty).
fn check_row(ui: &mut egui::Ui, f: &mut Map<String, Value>, label: &str, label_w: f32, checks: &[(&str, &str)]) {
    ui.horizontal(|ui| {
        widgets::form_label(ui, label, label_w);
        for (i, (key, text)) in checks.iter().enumerate() {
            if i > 0 {
                ui.add_space(10.0);
            }
            check(ui, f, key, text);
        }
    });
}

pub fn body(app: &mut PhotospliceApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    // The monitor profile in use can change while the dialog is open.
    f.insert("__note".into(), json!(crate::monitor_status::note(app)));
    let t = Tokens::get(ui.ctx());
    let profiles = installed();
    let any_class = [ProfileClass::Input, ProfileClass::Display, ProfileClass::Output, ProfileClass::ColorSpace];
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LEFT_W);
            let label_w = widgets::form_label_width(ui, ["RGB:", "CMYK:", "Gray:", "Profile Mismatches:", "Missing Profiles:"], 60.0, 150.0);
            let menu_w = LEFT_W - label_w - 40.0;
            crate::prefs_ui::pref_group(ui, "Working Spaces", |ui| {
                let rgb = builtins(&["srgb", "display-p3", "adobe-rgb-compat", "prophoto-compat", "linear-srgb", "rec2020"]);
                menu_row(ui, f, "RGB:", label_w, "workingRgb", profile_menu(rgb, &profiles, ColorSpace::Rgb, &any_class), menu_w);
                let mut cmyk = vec![
                    ("auto".to_string(), "Automatic (Coated FOGRA39 if installed)"),
                    ("coated-cmyk".to_string(), "PhotoSplice Coated CMYK (built-in)"),
                ];
                cmyk = profile_menu(cmyk, &profiles, ColorSpace::Cmyk, &any_class);
                menu_row(ui, f, "CMYK:", label_w, "workingCmyk", cmyk, menu_w);
                let gray = builtins(&["dot-gain-20-compat", "sgray", "gray-gamma-2.2"]);
                menu_row(ui, f, "Gray:", label_w, "workingGray", profile_menu(gray, &profiles, ColorSpace::Gray, &any_class), menu_w);
            });
            crate::prefs_ui::pref_group(ui, "Color Management Policies", |ui| {
                for (key, label, convert) in [
                    ("policyRgb", "RGB:", "Convert to Working RGB"),
                    ("policyCmyk", "CMYK:", "Convert to Working CMYK"),
                    ("policyGray", "Gray:", "Convert to Working Gray"),
                ] {
                    let opts = vec![("off".to_string(), "Off"), ("preserve".to_string(), "Preserve Embedded Profiles"), ("convert".to_string(), convert)];
                    menu_row(ui, f, label, label_w, key, opts, 220.0);
                }
                ui.add_space(4.0);
                check_row(ui, f, "Profile Mismatches:", label_w, &[("askOnMismatch", "Ask When Opening"), ("askOnPaste", "Ask When Pasting")]);
                check_row(ui, f, "Missing Profiles:", label_w, &[("askOnMissing", "Ask When Opening")]);
            });
        });
        ui.add_space(GAP);
        ui.vertical(|ui| {
            ui.set_width(WIDTH - LEFT_W - GAP);
            let label_w = widgets::form_label_width(ui, ["Intent:", "Monitor:"], 40.0, 110.0);
            crate::prefs_ui::pref_group(ui, "Conversion Options", |ui| {
                let intents = vec![
                    ("perceptual".to_string(), "Perceptual"),
                    ("saturation".to_string(), "Saturation"),
                    ("relative".to_string(), "Relative Colorimetric"),
                    ("absolute".to_string(), "Absolute Colorimetric"),
                ];
                menu_row(ui, f, "Intent:", label_w, "intent", intents, 190.0);
                check_row(ui, f, "", label_w, &[("bpc", "Use Black Point Compensation")]);
                check_row(ui, f, "", label_w, &[("dither", "Use Dither (8-bit/channel images)")]);
            });
            crate::prefs_ui::pref_group(ui, "Advanced Controls", |ui| text_gamma_row(ui, f));
            crate::prefs_ui::pref_group(ui, "Monitor", |ui| {
                let mut monitor = vec![("auto".to_string(), "Automatic (the display's profile)")];
                monitor.extend(builtins(&["srgb", "display-p3", "adobe-rgb-compat", "prophoto-compat", "rec2020"]));
                let monitor = profile_menu(monitor, &profiles, ColorSpace::Rgb, &[ProfileClass::Display]);
                menu_row(ui, f, "Monitor:", label_w, "monitorProfile", monitor, 190.0);
                ui.add_space(2.0);
                ui.label(egui::RichText::new(s(f, "__note")).color(t.text_faint).size(11.5));
            });
        });
    });
}

/// Advanced Controls › Blend Text Colors Using Gamma: the box turns it off (gamma 1) and back on
/// at the value it had; the field takes 1.00–2.20.
fn text_gamma_row(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let gamma = f.get("blendTextGamma").and_then(Value::as_f64).filter(|g| g.is_finite()).unwrap_or(TEXT_GAMMA);
    let mut on = gamma > 1.0 + 1e-4;
    ui.horizontal(|ui| {
        if widgets::checkbox(ui, &mut on, "Blend Text Colors Using Gamma:").changed() {
            let next = if on {
                f.get("__textGamma").and_then(Value::as_f64).filter(|g| *g > 1.0).unwrap_or(TEXT_GAMMA)
            } else {
                f.insert("__textGamma".into(), json!(gamma));
                1.0
            };
            f.insert("blendTextGamma".into(), json!(next));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut v = f.get("blendTextGamma").and_then(Value::as_f64).unwrap_or(TEXT_GAMMA) as f32;
            let changed = ui.add_enabled_ui(on, |ui| widgets::value_field_prec(ui, &mut v, 1.0..=2.2, "", 56.0, 2).changed()).inner;
            if changed {
                f.insert("blendTextGamma".into(), json!((f64::from(v) * 100.0).round() / 100.0));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The box turns the text gamma off (1) and back on at the value it had.
    #[test]
    fn text_gamma_box_keeps_the_value_it_turns_off() {
        use egui_kittest::kittest::Queryable;
        let mut f = Map::new();
        f.insert("blendTextGamma".into(), json!(1.8));
        // Drawn once the theme's fonts are in (the harness runs a frame as it is built).
        let ready = std::rc::Rc::new(std::cell::Cell::new(false));
        let drawn = ready.clone();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(400.0, 100.0)).build_ui_state(
            move |ui, f: &mut Map<String, Value>| {
                if drawn.get() {
                    text_gamma_row(ui, f);
                }
            },
            f,
        );
        PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::default());
        ready.set(true);
        h.run();
        h.get_by_label("Blend Text Colors Using Gamma:").click();
        h.run();
        assert_eq!(h.state()["blendTextGamma"], json!(1.0));
        h.get_by_label("Blend Text Colors Using Gamma:").click();
        h.run();
        assert_eq!(h.state()["blendTextGamma"], json!(1.8));
    }

    #[test]
    fn owns_only_color_settings() {
        let mut f = Map::new();
        assert!(!owns(&f));
        f.insert("__command".into(), json!(COMMAND));
        assert!(owns(&f));
    }

    /// The menus name profiles by description, and a profile path that isn't listed still shows.
    #[test]
    fn menus_name_builtins_and_keep_an_unlisted_path() {
        assert_eq!(builtins(&["srgb"]), vec![("srgb".to_string(), "sRGB IEC61966-2.1")]);
        let none: Vec<InstalledProfile> = Vec::new();
        let opts = profile_menu(builtins(&["sgray"]), &none, ColorSpace::Gray, &[ProfileClass::Display]);
        assert_eq!(opts.len(), 1);
    }
}
