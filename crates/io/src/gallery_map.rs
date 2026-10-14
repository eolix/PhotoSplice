//! Filter Gallery smart filters in PSD.
//!
//! PSD stores a one-effect gallery as a `filterFX` item whose `Fltr` is a `GEfc` object:
//! `GEfk` names the effect (enum `GEft`, e.g. `Spng` for Sponge), then the effect's settings
//! under their own keys and, for effects that draw random numbers, the seed `FlRs`. The item's
//! `FrgC`/`BckC` hold the foreground and background colours the Sketch effects use.
//!
//! One-effect galleries map to `filter.filterGallery`; anything else (a stack of several effects)
//! stays a [`super::UNSUPPORTED_FILTER`] with its descriptor kept verbatim.

use photosuite_algo::{GalleryFilter as G, GalleryParamKind};
use photosuite_doc::SmartFilter;
use photosuite_psd::descriptor::{Descriptor, Id, UnicodeString, Value};
use serde_json::{Map, Value as J, json};

use crate::blocks::{enum_of, get_desc, num};

/// The `Fltr` class and `filterID` of a gallery smart filter.
pub(super) const CLASS: &str = "GEfc";
const FILTER_ID: i32 = i32::from_be_bytes(*b"GEfc");
const COMMAND: &str = "filter.filterGallery";

/// A choice parameter's enum type and its values, in the parameter's choice-name vocabulary.
struct Choices {
    type_id: &'static str,
    values: &'static [(&'static str, &'static str)],
}

const TEXTURE: Choices = Choices {
    type_id: "TxtT",
    values: &[("blocks", "TxBl"), ("canvas", "TxCa"), ("frosted", "TxFr"), ("tinyLens", "TxTL"), ("brick", "TxBr"), ("burlap", "TxBu"), ("sandstone", "TxSt")],
};
const LIGHT: Choices = Choices {
    type_id: "LghD",
    values: &[
        ("bottom", "LDBt"),
        ("bottomLeft", "LDBL"),
        ("left", "LDLf"),
        ("topLeft", "LDTL"),
        ("top", "LDTp"),
        ("topRight", "LDTR"),
        ("right", "LDRg"),
        ("bottomRight", "LDBR"),
    ],
};
const LIGHT_POSITION: Choices = Choices {
    type_id: "LghP",
    values: &[
        ("bottom", "LPBt"),
        ("bottomLeft", "LPBL"),
        ("left", "LPLf"),
        ("topLeft", "LPTL"),
        ("top", "LPTp"),
        ("topRight", "LPTR"),
        ("right", "LPRg"),
        ("bottomRight", "LPBR"),
    ],
};
const BRUSH_TYPE: Choices = Choices {
    type_id: "BrsT",
    values: &[("simple", "BrSm"), ("lightRough", "BrsL"), ("darkRough", "BrDR"), ("wideSharp", "BrsW"), ("wideBlurry", "BrbW"), ("sparkle", "BrSp")],
};
const STROKE_DIRECTION: Choices =
    Choices { type_id: "StrD", values: &[("rightDiagonal", "SDRD"), ("horizontal", "SDHz"), ("leftDiagonal", "SDLD"), ("vertical", "SDVt")] };
const SCREEN: Choices = Choices { type_id: "ScrT", values: &[("circle", "ScrC"), ("dot", "ScrD"), ("line", "ScrL")] };
const GRAIN: Choices = Choices {
    type_id: "Grnt",
    values: &[
        ("regular", "GrnR"),
        ("soft", "GrSf"),
        ("sprinkles", "GrSr"),
        ("clumped", "GrnC"),
        ("contrasty", "GrCn"),
        ("enlarged", "GrnE"),
        ("stippled", "GrSt"),
        ("horizontal", "GrnH"),
        ("vertical", "GrnV"),
        ("speckle", "GrSp"),
    ],
};

/// How one parameter is stored.
#[derive(Clone, Copy)]
enum Kind {
    /// An integer slider (or a 0/1 flag stored as a boolean when `bool`).
    Number,
    Bool,
    Choice(&'static Choices),
    /// The Neon Glow colour, an `RGBC` object with 0–255 components.
    Colour,
}

/// One gallery effect: its `GEft` id and its (PSD key, parameter key, kind) settings.
struct Effect {
    filter: G,
    id: &'static str,
    params: &'static [(&'static str, &'static str, Kind)],
}

use Kind::{Bool, Choice, Colour, Number};

const RELIEF_SET: [(&str, &str, Kind); 5] = [
    ("TxtT", "texture", Choice(&TEXTURE)),
    ("Scln", "scaling", Number),
    ("Rlf ", "relief", Number),
    ("LghD", "light", Choice(&LIGHT)),
    ("InvT", "invert", Bool),
];

const EFFECTS: &[Effect] = &[
    Effect {
        filter: G::ColoredPencil,
        id: "ClrP",
        params: &[("Pncl", "pencilWidth", Number), ("StrP", "strokePressure", Number), ("PprB", "paperBrightness", Number)],
    },
    Effect {
        filter: G::Cutout,
        id: "Ct  ",
        params: &[("NmbL", "numberOfLevels", Number), ("EdgS", "edgeSimplicity", Number), ("EdgF", "edgeFidelity", Number)],
    },
    Effect { filter: G::DryBrush, id: "DryB", params: &[("BrsS", "brushSize", Number), ("BrsD", "brushDetail", Number), ("Txtr", "texture", Number)] },
    Effect { filter: G::FilmGrain, id: "FlmG", params: &[("Grn ", "grain", Number), ("HghA", "highlightArea", Number), ("Intn", "intensity", Number)] },
    Effect { filter: G::Fresco, id: "Frsc", params: &[("BrsS", "brushSize", Number), ("BrsD", "brushDetail", Number), ("Txtr", "texture", Number)] },
    Effect { filter: G::NeonGlow, id: "NGlw", params: &[("Sz  ", "glowSize", Number), ("Brgh", "glowBrightness", Number), ("Clr ", "glowColor", Colour)] },
    Effect {
        filter: G::PaintDaubs,
        id: "PntD",
        params: &[("Sz  ", "brushSize", Number), ("Shrp", "sharpness", Number), ("BrsT", "brushType", Choice(&BRUSH_TYPE))],
    },
    Effect { filter: G::PaletteKnife, id: "PltK", params: &[("StrS", "strokeSize", Number), ("StDt", "strokeDetail", Number), ("Sftn", "softness", Number)] },
    Effect { filter: G::PlasticWrap, id: "PlsW", params: &[("HghS", "highlightStrength", Number), ("Dtl ", "detail", Number), ("Smth", "smoothness", Number)] },
    Effect {
        filter: G::PosterEdges,
        id: "PstE",
        params: &[("EdgT", "edgeThickness", Number), ("EdgI", "edgeIntensity", Number), ("Pstr", "posterization", Number)],
    },
    Effect {
        filter: G::RoughPastels,
        id: "RghP",
        params: &[
            ("StrL", "strokeLength", Number),
            ("StDt", "strokeDetail", Number),
            RELIEF_SET[0],
            RELIEF_SET[1],
            RELIEF_SET[2],
            RELIEF_SET[3],
            RELIEF_SET[4],
        ],
    },
    Effect {
        filter: G::SmudgeStick,
        id: "SmdS",
        params: &[("StrL", "strokeLength", Number), ("HghA", "highlightArea", Number), ("Intn", "intensity", Number)],
    },
    Effect { filter: G::Sponge, id: "Spng", params: &[("BrsS", "brushSize", Number), ("Dfnt", "definition", Number), ("Smth", "smoothness", Number)] },
    Effect {
        filter: G::Underpainting,
        id: "Undr",
        params: &[
            ("BrsS", "brushSize", Number),
            ("TxtC", "textureCoverage", Number),
            RELIEF_SET[0],
            RELIEF_SET[1],
            RELIEF_SET[2],
            RELIEF_SET[3],
            RELIEF_SET[4],
        ],
    },
    Effect { filter: G::Watercolor, id: "Wtrc", params: &[("BrsD", "brushDetail", Number), ("ShdI", "shadowIntensity", Number), ("Txtr", "texture", Number)] },
    Effect {
        filter: G::AccentedEdges,
        id: "AccE",
        params: &[("EdgW", "edgeWidth", Number), ("EdgB", "edgeBrightness", Number), ("Smth", "smoothness", Number)],
    },
    Effect {
        filter: G::AngledStrokes,
        id: "AngS",
        params: &[("DrcB", "directionBalance", Number), ("StrL", "strokeLength", Number), ("Shrp", "sharpness", Number)],
    },
    Effect { filter: G::Crosshatch, id: "Crsh", params: &[("StrL", "strokeLength", Number), ("Shrp", "sharpness", Number), ("Strg", "strength", Number)] },
    Effect {
        filter: G::DarkStrokes,
        id: "DrkS",
        params: &[("Blnc", "balance", Number), ("BlcI", "blackIntensity", Number), ("WhtI", "whiteIntensity", Number)],
    },
    Effect {
        filter: G::InkOutlines,
        id: "InkO",
        params: &[("StrL", "strokeLength", Number), ("DrkI", "darkIntensity", Number), ("LghI", "lightIntensity", Number)],
    },
    Effect { filter: G::Spatter, id: "Spt ", params: &[("SprR", "sprayRadius", Number), ("Smth", "smoothness", Number)] },
    Effect {
        filter: G::SprayedStrokes,
        id: "SprS",
        params: &[("StrL", "strokeLength", Number), ("SprR", "sprayRadius", Number), ("SDir", "strokeDirection", Choice(&STROKE_DIRECTION))],
    },
    Effect { filter: G::SumiE, id: "Smie", params: &[("StrW", "strokeWidth", Number), ("StrP", "strokePressure", Number), ("Cntr", "contrast", Number)] },
    Effect { filter: G::DiffuseGlow, id: "DfsG", params: &[("Grns", "graininess", Number), ("GlwA", "glowAmount", Number), ("ClrA", "clearAmount", Number)] },
    Effect {
        filter: G::Glass,
        id: "Gls ",
        params: &[("Dstr", "distortion", Number), ("Smth", "smoothness", Number), RELIEF_SET[0], RELIEF_SET[1], RELIEF_SET[4]],
    },
    Effect { filter: G::OceanRipple, id: "OcnR", params: &[("RplS", "rippleSize", Number), ("RplM", "rippleMagnitude", Number)] },
    Effect { filter: G::BasRelief, id: "BsRl", params: &[("Dtl ", "detail", Number), ("Smth", "smoothness", Number), RELIEF_SET[3]] },
    Effect {
        filter: G::ChalkCharcoal,
        id: "ChlC",
        params: &[("ChrA", "charcoalArea", Number), ("ChlA", "chalkArea", Number), ("StrP", "strokePressure", Number)],
    },
    Effect {
        filter: G::Charcoal,
        id: "Chrc",
        params: &[("ChAm", "charcoalThickness", Number), ("Dtl ", "detail", Number), ("LgDr", "lightDarkBalance", Number)],
    },
    Effect { filter: G::Chrome, id: "Chrm", params: &[("Dtl ", "detail", Number), ("Smth", "smoothness", Number)] },
    Effect {
        filter: G::ConteCrayon,
        id: "CntC",
        params: &[
            ("FrgL", "foregroundLevel", Number),
            ("BckL", "backgroundLevel", Number),
            RELIEF_SET[0],
            RELIEF_SET[1],
            RELIEF_SET[2],
            RELIEF_SET[3],
            RELIEF_SET[4],
        ],
    },
    Effect {
        filter: G::GraphicPen,
        id: "GraP",
        params: &[("StrL", "strokeLength", Number), ("LgDr", "lightDarkBalance", Number), ("SDir", "strokeDirection", Choice(&STROKE_DIRECTION))],
    },
    Effect {
        filter: G::HalftonePattern,
        id: "HlfS",
        params: &[("HlSz", "size", Number), ("Cntr", "contrast", Number), ("ScrT", "patternType", Choice(&SCREEN))],
    },
    Effect { filter: G::NotePaper, id: "NtPr", params: &[("ImgB", "imageBalance", Number), ("Grns", "graininess", Number), ("Rlf ", "relief", Number)] },
    Effect { filter: G::Photocopy, id: "Phtc", params: &[("Dtl ", "detail", Number), ("Drkn", "darkness", Number)] },
    Effect {
        filter: G::Plaster,
        id: "Plst",
        params: &[("ImgB", "imageBalance", Number), ("Smth", "smoothness", Number), ("LghP", "light", Choice(&LIGHT_POSITION))],
    },
    Effect {
        filter: G::Reticulation,
        id: "Rtcl",
        params: &[("Dnst", "density", Number), ("BlcL", "foregroundLevel", Number), ("WhtL", "backgroundLevel", Number)],
    },
    Effect { filter: G::Stamp, id: "Stmp", params: &[("LgDr", "lightDarkBalance", Number), ("Smth", "smoothness", Number)] },
    Effect { filter: G::TornEdges, id: "TrnE", params: &[("ImgB", "imageBalance", Number), ("Smth", "smoothness", Number), ("Cntr", "contrast", Number)] },
    Effect { filter: G::WaterPaper, id: "WtrP", params: &[("FbrL", "fiberLength", Number), ("Brgh", "brightness", Number), ("Cntr", "contrast", Number)] },
    Effect {
        filter: G::GlowingEdges,
        id: "GlwE",
        params: &[("EdgW", "edgeWidth", Number), ("EdgB", "edgeBrightness", Number), ("Smth", "smoothness", Number)],
    },
    Effect {
        filter: G::Craquelure,
        id: "Crql",
        params: &[("CrcS", "crackSpacing", Number), ("CrcD", "crackDepth", Number), ("CrcB", "crackBrightness", Number)],
    },
    Effect { filter: G::Grain, id: "Grn ", params: &[("Intn", "intensity", Number), ("Cntr", "contrast", Number), ("Grnt", "grainType", Choice(&GRAIN))] },
    Effect { filter: G::MosaicTiles, id: "MscT", params: &[("TlSz", "tileSize", Number), ("GrtW", "groutWidth", Number), ("LghG", "lightenGrout", Number)] },
    Effect { filter: G::Patchwork, id: "Ptch", params: &[("SqrS", "squareSize", Number), ("Rlf ", "relief", Number)] },
    Effect {
        filter: G::StainedGlass,
        id: "StnG",
        params: &[("ClSz", "cellSize", Number), ("BrdT", "borderThickness", Number), ("LghI", "lightIntensity", Number)],
    },
    Effect { filter: G::Texturizer, id: "Txtz", params: &RELIEF_SET },
];

fn by_id(id: &[u8]) -> Option<&'static Effect> {
    EFFECTS.iter().find(|e| e.id.as_bytes() == id)
}

fn by_filter(f: G) -> Option<&'static Effect> {
    EFFECTS.iter().find(|e| e.filter == f)
}

/// An `RGBC` object as `[r, g, b]` in 0–1.
fn rgb_of(d: Option<&Descriptor>) -> Option<J> {
    let d = d?;
    let c = |k: &str| num(d.get(k)).filter(|v| v.is_finite()).map(|v| (v / 255.0).clamp(0.0, 1.0));
    Some(json!([c("Rd  ")?, c("Grn ")?, c("Bl  ")?]))
}

fn rgbc_from(p: Option<&J>, default: f64) -> Value {
    let c = |i: usize| p.and_then(|a| a.get(i)).and_then(J::as_f64).filter(|v| v.is_finite()).map_or(default, |v| (v.clamp(0.0, 1.0) * 255.0).round());
    Value::Descriptor(Descriptor::new("RGBC").with("Rd  ", Value::Double(c(0))).with("Grn ", Value::Double(c(1))).with("Bl  ", Value::Double(c(2))))
}

/// `filter.filterGallery` and its params for a one-effect `GEfc` (`None` when it isn't one).
pub(super) fn import(item: &Descriptor, fltr: &Descriptor) -> Option<(&'static str, J)> {
    let effect = by_id(enum_of(fltr, "GEfk")?)?;
    let mut params = Map::new();
    for (psd, key, kind) in effect.params {
        let Some(v) = fltr.get(psd) else { continue };
        let value = match (kind, v) {
            (Number, v) => json!(num(Some(v)).filter(|x| x.is_finite())?),
            (Bool, Value::Boolean(b)) => json!(b),
            (Choice(c), Value::Enumerated { value, .. }) => json!(c.values.iter().find(|(_, id)| id.as_bytes() == value.as_bytes())?.0),
            (Colour, Value::Descriptor(d)) => rgb_of(Some(d))?,
            _ => return None,
        };
        params.insert((*key).into(), value);
    }
    if let Some(seed) = fltr.get("FlRs").and_then(|v| num(Some(v))).filter(|v| v.is_finite()) {
        params.insert("seed".into(), json!(seed as i64 as u32));
    }
    let mut out = json!({"effects": [{"filter": effect.filter.key(), "params": params}]});
    for (k, psd) in [("foreground", "FrgC"), ("background", "BckC")] {
        if let Some(c) = rgb_of(get_desc(item, psd)) {
            out[k] = c;
        }
    }
    Some((COMMAND, out))
}

/// The one effect of a gallery smart filter, with the colours, or why it can't be written.
fn single_effect(f: &SmartFilter) -> Result<(G, J), String> {
    if let Some(key) = f.command.strip_prefix("filter.gallery.") {
        let g = G::from_key(key).ok_or("not a gallery effect")?;
        return Ok((g, f.params.clone()));
    }
    let list: Vec<&J> = f
        .params
        .get("effects")
        .and_then(J::as_array)
        .map(|a| a.iter().filter(|e| e.get("visible").and_then(J::as_bool) != Some(false)).collect())
        .unwrap_or_default();
    let [one] = list.as_slice() else {
        return Err("a Filter Gallery stack of several effects is not written to PSD yet".into());
    };
    let g = one.get("filter").and_then(J::as_str).and_then(G::from_key).ok_or("unknown gallery effect")?;
    let mut p = one.get("params").filter(|v| v.is_object()).cloned().unwrap_or_else(|| (*one).clone());
    for k in ["foreground", "background"] {
        if p.get(k).is_none()
            && let Some(c) = f.params.get(k)
            && let Some(o) = p.as_object_mut()
        {
            o.insert(k.into(), c.clone());
        }
    }
    Ok((g, p))
}

/// The `filterFXList` item of a gallery smart filter; `None` when `f` isn't one.
pub(super) fn export(f: &SmartFilter) -> Option<Result<Descriptor, String>> {
    if f.command != COMMAND && !f.command.starts_with("filter.gallery.") {
        return None;
    }
    Some(single_effect(f).and_then(|(g, p)| {
        let effect = by_filter(g).ok_or("no PSD equivalent")?;
        let defaults = photosuite_algo::GalleryEffect::new(g);
        let mut d = Descriptor::new(CLASS).with("GEfk", Value::Enumerated { type_id: Id::new("GEft"), value: Id::new(effect.id) });
        let kinds = g.params();
        for (psd, key, kind) in effect.params {
            let given = p.get(*key);
            let v = match kind {
                Number => Value::Integer(given.and_then(J::as_f64).filter(|v| v.is_finite()).unwrap_or(f64::from(defaults.get(key))).round() as i32),
                Bool => Value::Boolean(given.and_then(J::as_bool).unwrap_or(defaults.flag(key))),
                Choice(c) => {
                    let name = given.and_then(J::as_str).unwrap_or_else(|| defaults.choice(key));
                    let known = kinds.iter().any(|k| k.key == *key && matches!(&k.kind, GalleryParamKind::Choice(n) if n.contains(&name)));
                    let id = c.values.iter().find(|(n, _)| *n == name && known).ok_or_else(|| format!("unknown {key} `{name}`"))?.1;
                    Value::Enumerated { type_id: Id::new(c.type_id), value: Id::new(id) }
                }
                Colour => rgbc_from(given.or(p.get("glowColor")), 0.0),
            };
            d = d.with(psd, v);
        }
        let seed = p.get("seed").and_then(J::as_u64).and_then(|v| u32::try_from(v).ok()).unwrap_or_else(|| g.default_seed());
        if g.default_seed() != 0 || p.get("seed").is_some() {
            d = d.with("FlRs", Value::Integer(seed as i32));
        }
        Ok(Descriptor::new("filterFX")
            .with("Nm  ", Value::Text(UnicodeString::new_nul("Filter Gallery...")))
            .with("blendOptions", super::blend_options(f))
            .with("enab", Value::Boolean(f.visible))
            .with("hasoptions", Value::Boolean(true))
            .with("FrgC", rgbc_from(p.get("foreground"), 0.0))
            .with("BckC", rgbc_from(p.get("background"), 255.0))
            .with("Fltr", Value::Descriptor(d))
            .with("filterID", Value::Integer(FILTER_ID)))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gallery_filter_has_its_parameters_mapped() {
        for f in G::ALL {
            let e = by_filter(*f).unwrap_or_else(|| panic!("{} has no GEft id", f.key()));
            let ours: Vec<&str> = f.params().iter().map(|p| p.key).collect();
            for (_, key, _) in e.params {
                assert!(ours.contains(key) || *key == "glowColor", "{}: {key} is not a parameter", f.key());
            }
            for k in &ours {
                assert!(e.params.iter().any(|(_, key, _)| key == k), "{}: {k} has no PSD key", f.key());
            }
        }
    }

    /// The descriptor the reference wrote for Sponge at its defaults (as a smart filter).
    fn reference_sponge() -> Descriptor {
        Descriptor::new("filterFX")
            .with("Nm  ", Value::Text(UnicodeString::new_nul("Filter Gallery...")))
            .with("FrgC", rgbc_from(None, 0.0))
            .with("BckC", rgbc_from(None, 255.0))
            .with(
                "Fltr",
                Value::Descriptor(
                    Descriptor::new("GEfc")
                        .with("GEfk", Value::Enumerated { type_id: Id::new("GEft"), value: Id::new("Spng") })
                        .with("BrsS", Value::Integer(2))
                        .with("Dfnt", Value::Integer(12))
                        .with("Smth", Value::Integer(5))
                        .with("FlRs", Value::Integer(250_816_192)),
                ),
            )
            .with("filterID", Value::Integer(FILTER_ID))
    }

    #[test]
    fn sponge_round_trips_with_its_seed() {
        let item = reference_sponge();
        let fltr = get_desc(&item, "Fltr").unwrap();
        let (cmd, p) = import(&item, fltr).unwrap();
        assert_eq!(cmd, COMMAND);
        let e = &p["effects"][0];
        assert_eq!(e["filter"], "sponge");
        assert_eq!(e["params"]["definition"], 12.0);
        assert_eq!(e["params"]["seed"], 250_816_192);
        let sf = SmartFilter { command: cmd.into(), params: p, blend: photosuite_color::BlendMode::Normal, opacity: 1.0, visible: true };
        let back = export(&sf).unwrap().unwrap();
        let f2 = get_desc(&back, "Fltr").unwrap();
        assert_eq!(f2, fltr);
    }

    #[test]
    fn choices_and_colours_round_trip() {
        let sf = SmartFilter {
            command: COMMAND.into(),
            params: json!({"effects": [{"filter": "texturizer", "params": {"texture": "sandstone", "light": "bottomLeft", "invert": true, "relief": 12}}], "foreground": [1.0, 0.0, 0.0]}),
            blend: photosuite_color::BlendMode::Normal,
            opacity: 1.0,
            visible: true,
        };
        let item = export(&sf).unwrap().unwrap();
        let (_, p) = import(&item, get_desc(&item, "Fltr").unwrap()).unwrap();
        let q = &p["effects"][0]["params"];
        assert_eq!(
            (q["texture"].as_str(), q["light"].as_str(), q["invert"].as_bool(), q["relief"].as_f64()),
            (Some("sandstone"), Some("bottomLeft"), Some(true), Some(12.0))
        );
        assert_eq!(p["foreground"], json!([1.0, 0.0, 0.0]));
    }

    #[test]
    fn stacks_and_strangers_are_not_mapped() {
        let two = SmartFilter {
            command: COMMAND.into(),
            params: json!({"effects": [{"filter": "sponge"}, {"filter": "grain"}]}),
            blend: photosuite_color::BlendMode::Normal,
            opacity: 1.0,
            visible: true,
        };
        assert!(export(&two).unwrap().is_err());
        let odd = Descriptor::new("GEfc").with("GEfk", Value::Enumerated { type_id: Id::new("GEft"), value: Id::new("Zzzz") });
        assert!(import(&Descriptor::new("filterFX"), &odd).is_none());
        let bad = Descriptor::new("GEfc")
            .with("GEfk", Value::Enumerated { type_id: Id::new("GEft"), value: Id::new("Grn ") })
            .with("Grnt", Value::Enumerated { type_id: Id::new("Grnt"), value: Id::new("Zzzz") });
        assert!(import(&Descriptor::new("filterFX"), &bad).is_none(), "an unknown choice keeps the raw descriptor");
    }
}
