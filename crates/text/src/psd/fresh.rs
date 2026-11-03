//! A document `Txt2` made from the type layers alone, for a document that has none to patch (one
//! made here, or one whose block no longer describes its layers). Reference reads a type layer
//! from `Txt2` when the file has one, and only there finds what `TySh` can't say: a variable
//! font's instance (its axis values beside the font's name), the automatic kerning mode, the
//! OpenType settings EngineData leaves out. Without the block it rebuilds every layer from
//! `TySh`, and a variable font opens in its default instance.
//!
//! The block is shaped as Reference 2026 writes it for type it sets itself (version 14; every
//! file of the oracle corpus): the document resources (`/0`: font list, the CJK composition and
//! line-break sets, the character and paragraph sheets, the list styles, one frame per text
//! object) and the document objects (`/1`: the type engine's version, the document settings, the
//! text objects, the default character and paragraph style). Each text object's model takes its
//! text, style runs and paragraph runs from the layer's written `TySh` (through the same key maps
//! the patching uses, [`TXT2_STYLE_KEYS`] and [`TXT2_PARAGRAPH_KEYS`]); its view names its frame
//! and holds an empty layout, which Reference composes again on open. The constant tables are
//! the values Reference writes into every such block (the numbered keys are those of its older,
//! named form: `/DocumentResources /FontSet /MojiKumiCodeToClassSet /MojiKumiTableSet
//! /KinsokuSet /StyleSheetSet /ParagraphSheetSet /TextFrameSet /ListStyleSet`, `/DocumentObjects
//! /DocumentSettings /TextObjects /OriginalNormalStyleFeatures /OriginalNormalParagraphFeatures`).

use super::{
    E, RunKind, TXT2_FEATURES, TXT2_STYLISTIC_SETS, box_bounds, box_frame_points, ed, ed_runs, engine_data, new_frame, parse_tysh, postscript_for,
    set_txt2_attr, text_index, txt2_mode_runs,
};
use crate::fonts::FontDetails;
use photosplice_doc::TextLayer;
use photosplice_doc::text::{AntiAlias, CharStyle, Orientation, TextShape};

/// The faces Reference lists after a document's own: the default style's font, the font it shows
/// hidden characters in, and one more it always names.
const DEFAULT_FONT: &str = "MyriadPro-Regular";
const HIDDEN_GLYPH_FONT: &str = "AdobeInvisFont";
const EXTRA_FONT: &str = "TimesNewRomanPSMT";

/// The normal character style (`/0 /5` "Normal RGB", `/1 /2`), its font (`/0`) set apart.
const NORMAL_STYLE: &str = "<< /0 0 /1 12.0 /2 false /3 false /4 true /5 0.0 /6 1.0 /7 1.0 /8 0 /9 0.0 /10 0.0 /11 1 /12 0 /13 0 /14 0 /15 0 /16 0 \
/17 0.0 /18 true /19 false /20 false /21 false /22 false /23 false /24 false /25 false /26 false /27 false /28 false /29 false /30 0 /31 false /32 false \
/33 false /34 false /35 2 /36 0.0 /37 2 /38 0 /39 0 /40 false /41 2 /42 0 /43 << /0 .5 >> /44 2 /45 2 /46 7 /47 0 /48 0 /49 -1.0 /50 -1.0 /51 0 \
/52 false /53 << /99 /SimplePaint /0 << /0 1 /1 [ 1.0 0.0 0.0 0.0 ] >> >> /54 << /99 /SimplePaint /0 << /0 1 /1 [ 1.0 0.0 0.0 0.0 ] >> >> \
/55 << /99 /SimpleBlender >> /56 true /57 false /58 true /59 false /60 false /61 0 /62 0 /63 1.0 /64 4.0 /65 0.0 /66 [ ] /67 [ ] /68 0 /69 0 /70 0 \
/71 4 /72 0.0 /73 0.0 /74 false /75 false /76 false /77 true /78 true /79 << /99 /SimplePaint /0 << /0 1 /1 [ 1.0 1.0 1.0 0.0 ] >> >> /80 false \
/81 0 /82 3.0 /83 3.0 /84 false /85 0 /86 << /99 /SimpleCustomFeature >> /87 100.0 /88 true /91 1 /92 0.0 /95 << /0 0.0 /1 0 >> /96 true >>";

/// A style run's style as Reference writes it for type set in its default style, before the
/// run's own attributes are set on it.
const RUN_STYLE: &str = "<< /0 0 /1 12.0 /2 false /3 false /4 true /5 0.0 /6 1.0 /7 1.0 /8 0 /9 0.0 /11 1 /12 0 /13 0 /14 0 /15 0 /16 0 /18 true \
/19 false /20 false /23 false /24 false /25 false /26 false /27 false /28 false /29 false /30 0 /31 false /32 false /33 false /35 1 /36 0.0 /37 2 \
/38 0 /39 0 /52 false /53 << /99 /SimplePaint /0 << /0 1 /1 [ 1.0 0.0 0.0 0.0 ] >> >> /54 << /99 /SimplePaint /0 << /0 1 /1 [ 1.0 0.0 0.0 0.0 ] >> >> \
/68 0 /70 0 /72 0.0 /73 0.0 /75 false /85 0 /87 100.0 /88 false /92 0.0 >>";

/// The normal paragraph style (`/0 /6` "Normal RGB", `/1 /3`).
const NORMAL_PARAGRAPH: &str = "<< /0 0 /1 0.0 /2 0.0 /3 0.0 /4 0.0 /5 0.0 /6 1 /7 1.2 /8 0 /9 true /10 6 /11 2 /12 2 /13 0 /14 36.0 /15 true /16 .5 \
/17 [ .8 1.0 1.33 ] /18 [ 0.0 0.0 0.0 ] /19 [ 1.0 1.0 1.0 ] /20 6 /21 false /22 0 /23 true /24 0 /25 0 /27 /nil /26 false /28 /nil /29 false /30 << >> \
/31 36.0 /32 << >> /33 0 /34 7 /35 1 /36 /nil /37 0 /38 false /39 0 /40 2 /41 0 /47 false /48 /nil >>";

/// The "Basic Paragraph" sheet's character features (`/32`), its font (`/0`) set apart.
const BASIC_PARAGRAPH_STYLE: &str = "<< /0 0 /1 12.0 /2 false /3 false /4 true /5 0.0 /6 1.0 /7 1.0 /8 0 /9 0.0 /10 0.0 /11 1 /12 0 /13 0 /15 0 /16 0 \
/18 true /19 false /20 true /21 false /22 false /23 false /24 false /25 false /26 false /27 true /28 false /29 false /30 0 /35 1 /38 0 \
/53 << /99 /SimplePaint /0 << /0 1 /1 [ 1.0 0.0 0.0 0.0 ] >> >> /54 << /99 /SimplePaint /0 << /0 1 /1 [ 1.0 0.0 0.0 0.0 ] >> >> /68 0 /70 1 /71 4 \
/72 0.0 /73 0.0 /85 0 /87 0.0 >>";

/// A paragraph run's properties as Reference writes them for a new paragraph, before the run's
/// own are set on them (`/24` Burasagari and `/29` the Every-line Composer on, as new type has).
const RUN_PARAGRAPH: &str = "<< /0 0 /1 0.0 /2 0.0 /3 0.0 /4 0.0 /5 0.0 /7 1.2 /8 0 /9 true /17 [ .8 1.0 1.33 ] /18 [ 0.0 0.0 0.0 ] \
/19 [ 1.0 1.0 1.0 ] /21 false /24 1 /25 0 /27 /nil /28 /nil /29 true /33 0 /35 1 /36 /nil /37 0 /40 2 >>";

/// The document's font-use record (`/1 /5`), as every Reference 2026 block has it: two flag
/// strings, the faces' versions added per font.
const FONT_RECORD_FLAGS: (&str, &str) = ("0010000001001001111011100011100011101100000010000110000000", "01110");

/// The Japanese composition sets (`/0 /3`, `MojiKumiTableSet`): name and class.
const MOJIKUMI: [(&str, i64); 9] = [
    ("Photoshop6MojiKumiSet4", 2),
    ("Photoshop6MojiKumiSet3", 4),
    ("Photoshop6MojiKumiSet2", 3),
    ("Photoshop6MojiKumiSet1", 1),
    ("YakumonoHankaku", 1),
    ("GyomatsuYakumonoHankaku", 3),
    ("GyomatsuYakumonoZenkaku", 4),
    ("YakumonoZenkaku", 2),
    ("GyomatsuYakumonoZenkakuOrHankaku", 5),
];

const KINSOKU_HARD: [&str; 4] = [
    "!),.:;?]}\u{a2}\u{2014}\u{2019}\u{201d}\u{2030}\u{2103}\u{2109}\u{3001}\u{3002}\u{3005}\u{3009}\u{300b}\u{300d}\u{300f}\u{3011}\u{3015}\u{3041}\u{3043}\u{3045}\u{3047}\u{3049}\u{3063}\u{3083}\u{3085}\u{3087}\u{308e}\u{309b}\u{309c}\u{309d}\u{309e}\u{30a1}\u{30a3}\u{30a5}\u{30a7}\u{30a9}\u{30c3}\u{30e3}\u{30e5}\u{30e7}\u{30ee}\u{30f5}\u{30f6}\u{30fb}\u{30fc}\u{30fd}\u{30fe}\u{ff01}\u{ff05}\u{ff09}\u{ff0c}\u{ff0e}\u{ff1a}\u{ff1b}\u{ff1f}\u{ff3d}\u{ff5d}",
    "([{\u{a3}\u{a7}\u{2018}\u{201c}\u{3008}\u{300a}\u{300c}\u{300e}\u{3010}\u{3012}\u{3014}\u{ff03}\u{ff04}\u{ff08}\u{ff20}\u{ff3b}\u{ff5b}\u{ffe5}",
    "\u{2014}\u{2025}\u{2026}",
    "\u{3001}\u{3002}\u{ff0c}\u{ff0e}",
];

const KINSOKU_SOFT: [&str; 4] = [
    "\u{2019}\u{201d}\u{3001}\u{3002}\u{3005}\u{3009}\u{300b}\u{300d}\u{300f}\u{3011}\u{3015}\u{309d}\u{309e}\u{30fb}\u{30fd}\u{30fe}\u{ff01}\u{ff09}\u{ff0c}\u{ff0e}\u{ff1a}\u{ff1b}\u{ff1f}\u{ff3d}\u{ff5d}",
    "\u{2018}\u{201c}\u{3008}\u{300a}\u{300c}\u{300e}\u{3010}\u{3014}\u{ff08}\u{ff3b}\u{ff5b}",
    "\u{2014}\u{2025}\u{2026}",
    "\u{3001}\u{3002}\u{ff0c}\u{ff0e}",
];

const KINSOKU_SOFT_V2: [&str; 4] = [
    "\u{2019}\u{201d}\u{3001}\u{3002}\u{3005}\u{3009}\u{300b}\u{300d}\u{300f}\u{3011}\u{3015}\u{309d}\u{309e}\u{30fb}\u{30fd}\u{30fe}\u{ff01}\u{ff09}\u{ff0c}\u{ff0e}\u{ff1a}\u{ff1b}\u{ff1f}\u{ff3d}\u{ff5d}\u{3035}\u{301f}\u{3019}\u{3017})]}\u{2986}\u{bb}.,",
    "\u{2018}\u{201c}(\u{3014}[{\u{3008}\u{300a}\u{300c}\u{300e}\u{3010}\u{2985}\u{3018}\u{3016}\u{ab}\u{301d}\u{ff08}\u{ff3b}\u{ff5b}\u{3033}\u{3034}",
    "\u{2014}\u{2025}\u{2026}\u{2015}",
    "\u{3001}\u{3002}\u{ff0c}\u{ff0e}\u{3000}",
];

/// The predefined list styles (`/0 /9`, `ListStyleSet`): name and tag.
const LIST_STYLES: [(&str, i64); 11] = [
    ("kPredefinedNumericListStyleTag", 1),
    ("kPredefinedUppercaseAlphaListStyleTag", 2),
    ("kPredefinedLowercaseAlphaListStyleTag", 3),
    ("kPredefinedUppercaseRomanNumListStyleTag", 4),
    ("kPredefinedLowercaseRomanNumListStyleTag", 5),
    ("kPredefinedBulletListStyleTag", 6),
    ("kPredefinedDashBulletListStyleTag", 6),
    ("kPredefinedEmptyCircleBulletListStyleTag", 6),
    ("kPredefinedFilledSquareBulletListStyleTag", 6),
    ("kPredefinedSingleZeroPrefixNumericListStyleTag", 7),
    ("kPredefinedDoubleZeroPrefixNumericListStyleTag", 8),
];

/// How hidden characters show (`WhitespaceCharacterMapping`): each character and the glyph of
/// the hidden-glyph font standing for it.
const WHITESPACE: [(&str, &str); 7] = [(" ", "1"), ("\r", "6"), ("\t", "0"), ("\u{2029}", "5"), ("\u{3}", "5"), ("\u{3000}", "1"), ("\u{ad}", "3")];

/// The smart quotes per language (`SmartQuoteSets`, by language number): open and close double,
/// open and close single (empty: not set).
const SMART_QUOTES: [[&str; 4]; 46] = [
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{201d}", "\u{201d}", "\u{2019}", "\u{2019}"],
    ["\u{ab}", "\u{bb}", "\u{2018}", "\u{2019}"],
    ["\u{ab}", "\u{bb}", "\u{2018}", "\u{2019}"],
    ["\u{201e}", "\u{201c}", "\u{201a}", "\u{2018}"],
    ["\u{201e}", "\u{201c}", "\u{201a}", "\u{2018}"],
    ["\u{ab}", "\u{bb}", "\u{2039}", "\u{203a}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{ab}", "\u{bb}", "\u{2018}", "\u{2019}"],
    ["\u{ab}", "\u{bb}", "\u{2018}", "\u{2019}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{201d}", "\u{201d}", "\u{2019}", "\u{2019}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{201d}", "\u{201d}", "\u{2019}", "\u{2019}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{ab}", "\u{bb}", "\u{2018}", "\u{2019}"],
    ["\u{ab}", "\u{bb}", "\u{2018}", "\u{2019}"],
    ["\u{201e}", "\u{201c}", "\u{201a}", "\u{2018}"],
    ["\u{201e}", "\u{201c}", "\u{201a}", "\u{2018}"],
    ["\u{201e}", "\u{201c}", "\u{201a}", "\u{2018}"],
    ["\u{201e}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{201e}", "\u{201d}", "\u{201a}", "\u{2019}"],
    ["\u{ab}", "\u{bb}", "\u{2039}", "\u{203a}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{201e}", "\u{201d}", "\u{2019}", "\u{2019}"],
    ["\u{301d}", "\u{301e}", "", ""],
    ["\u{300c}", "\u{300d}", "", ""],
    ["\u{201e}", "\u{201c}", "\u{2018}", "\u{2019}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{201e}", "\u{201c}", "\u{201a}", "\u{2018}"],
    ["\u{ab}", "\u{bb}", "\u{2018}", "\u{2019}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{201e}", "\u{201c}", "\u{201a}", "\u{2018}"],
    ["\u{ab}", "\u{bb}", "\u{2018}", "\u{2019}"],
    ["\u{201e}", "\u{201c}", "\u{201a}", "\u{2018}"],
    ["\u{ab}", "\u{bb}", "<", ">"],
    ["\u{201e}", "\u{201c}", "\u{201a}", "\u{2018}"],
    ["\u{ab}", "\u{bb}", "<", ">"],
    ["\u{201e}", "\u{201c}", "\u{201a}", "\u{2018}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
    ["\u{ab}", "\u{bb}", "\u{2039}", "\u{203a}"],
    ["\u{201c}", "\u{201d}", "\u{2018}", "\u{2019}"],
];

fn dict(items: Vec<(&str, E)>) -> E {
    E::Dict(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn int(i: usize) -> E {
    E::Int(i64::try_from(i).unwrap_or(i64::MAX))
}

fn parsed(src: &str) -> Option<E> {
    ed::parse(src.as_bytes()).ok().filter(|v| matches!(v, E::Dict(_)))
}

/// A resource set: `<< /0 [ resources ] /1 [ << /0 i >> … ] >>`, the display list `order`.
fn resource_set(resources: Vec<E>, order: &[usize]) -> E {
    dict(vec![("0", E::Array(resources)), ("1", E::Array(order.iter().map(|i| dict(vec![("0", int(*i))])).collect()))])
}

/// Reference's type engine version record (`/1 /4`, and each text object's view): 7.11.0, under
/// the name the engine gives its host.
fn engine_version() -> E {
    dict(vec![("0", E::Int(7)), ("1", E::Int(11)), ("2", E::Int(0)), ("3", E::String("Photoshop".into()))])
}

/// A font list entry (`/0 /1 /0`): `<< /0 << /99 /CoolTypeFont /0 << /0 name /1 script /2 type
/// /3 synthetic /4 [ axes ] /5 version >> >> >>`, script and synthetic only when not 0, as
/// Reference writes them.
pub(super) fn font_list_entry(name: &str, script: i64, kind: i64, synthetic: i64, details: &FontDetails) -> E {
    let mut font = vec![("0", E::String(name.to_string()))];
    if script != 0 {
        font.push(("1", E::Int(script)));
    }
    font.push(("2", E::Int(kind)));
    if synthetic != 0 {
        font.push(("3", E::Int(synthetic)));
    }
    if !details.axes.is_empty() {
        font.push(("4", axis_values(&details.axes)));
    }
    if let Some(v) = details.version.as_ref().filter(|v| !v.is_empty()) {
        font.push(("5", E::String(v.clone())));
    }
    dict(vec![("0", dict(vec![("99", E::Name("CoolTypeFont".into())), ("0", dict(font))]))])
}

/// Axis values as `Txt2` keeps them: 16.16 fixed-point integers.
pub(super) fn axis_values(axes: &[f32]) -> E {
    E::Array(axes.iter().map(|v| E::Int((f64::from(*v) * 65536.0).round().clamp(-2_147_483_648.0, 2_147_483_647.0) as i64)).collect())
}

/// The kerning `mode` (`/11`: 0 manual, 1 metrics, 2 optical), and the OpenType flags and
/// Stylistic Sets of `s` ([`TXT2_FEATURES`], [`TXT2_STYLISTIC_SETS`]) on a `Txt2` style.
fn set_style_extras(style: &mut E, s: &CharStyle, mode: i64) {
    style.set("11", E::Int(mode));
    let feature = |tag: &str| s.features.iter().find(|f| f.tag == tag).map(|f| f.value > 0);
    // A style that doesn't set Contextual Alternates is drawn with them (shapers apply `calt` by
    // default, Reference only when this flag is on), so they are written on.
    for (key, tag) in TXT2_FEATURES {
        style.set(key, E::Bool(feature(tag).unwrap_or(tag == "calt")));
    }
    let sets = (0..20).filter(|n| feature(&format!("ss{:02}", n + 1)) == Some(true)).fold(0i64, |m, n| m | 1 << n);
    style.set(TXT2_STYLISTIC_SETS, E::Int(sets));
}

/// The style of the character run of `layer` covering UTF-16 offset `at` of its engine text.
fn style_at(layer: &TextLayer, at: usize) -> CharStyle {
    let mut units = 0usize;
    let mut byte = layer.text.len();
    for (b, ch) in layer.text.char_indices() {
        if units >= at {
            byte = b;
            break;
        }
        units += ch.len_utf16();
    }
    let runs = layer.char_runs();
    let mut end = 0usize;
    runs.iter()
        .find(|r| {
            end = end.saturating_add(r.len);
            byte < end
        })
        .or(runs.last())
        .map(|r| r.style.clone())
        .unwrap_or_default()
}

/// The font list of the block made for `objects`: the faces their style runs name, in order of
/// use, then Reference's own three. A face the document's earlier block (`previous`) lists keeps
/// that entry (Reference's version string and instance axes); another is made from the layer's
/// `FontSet` entry (script, font type, synthetic style) and `details`. Also the index of each
/// font the block's defaults name.
fn font_list(
    objects: &[(&TextLayer, &[u8])],
    previous: Option<&E>,
    details: &mut dyn FnMut(&CharStyle, &str) -> FontDetails,
) -> Option<(Vec<E>, usize, usize)> {
    let kept: &[E] = previous.and_then(|p| p.path(&["0", "1", "0"])).and_then(E::as_array).unwrap_or(&[]);
    let mut names: Vec<String> = Vec::new();
    let mut fonts: Vec<E> = Vec::new();
    for (layer, written) in objects {
        let e = engine_data(&parse_tysh(written)?.text)?;
        let set = e.path(&["ResourceDict", "FontSet"]).and_then(E::as_array).unwrap_or(&[]);
        for (_, attrs) in ed_runs(written, RunKind::Style)? {
            let Some(name) = attrs.iter().find(|a| a.0 == "FontName").and_then(|a| a.1.as_str()) else { continue };
            if name.is_empty() || names.iter().any(|n| n == name) {
                continue;
            }
            names.push(name.to_string());
            if let Some(entry) = kept.iter().find(|f| f.path(&["0", "0", "0"]).and_then(E::as_str) == Some(name)) {
                fonts.push(entry.clone());
                continue;
            }
            let fs = set.iter().find(|f| f.get("Name").and_then(E::as_str) == Some(name));
            let num = |key: &str, default: i64| fs.and_then(|f| f.get(key)).and_then(E::as_i64).unwrap_or(default);
            // The style that names the face: its family and axis settings find the face.
            let style = layer.char_runs().into_iter().map(|r| r.style).find(|s| postscript_for(s) == name).unwrap_or_default();
            let d = details(&style, name);
            fonts.push(font_list_entry(name, num("Script", 0), num("FontType", 1), num("Synthetic", 0), &d));
        }
    }
    let mut index_of = |name: &str, kind: i64| match names.iter().position(|n| n == name) {
        Some(i) => i,
        None => {
            names.push(name.to_string());
            fonts.push(font_list_entry(name, 0, kind, 0, &FontDetails::default()));
            names.len() - 1
        }
    };
    let normal = index_of(DEFAULT_FONT, 0);
    let hidden = index_of(HIDDEN_GLYPH_FONT, 0);
    index_of(EXTRA_FONT, 1);
    Some((fonts, normal, hidden))
}

/// The `Txt2` style runs of the layer written as `written`: one per EngineData style run (cut
/// or extended at the end to the layer's text: Reference's path type keeps an empty EngineData
/// text), split where the layer's automatic kerning mode changes ([`txt2_mode_runs`]: EngineData
/// marks a manual kern on a neighbouring character, `Txt2` on the character itself). Each is
/// [`RUN_STYLE`] with the run's attributes set on it (font as an index into `txt2`'s font list),
/// and the kerning mode and OpenType settings of the layer's character run there.
fn style_runs(txt2: &E, layer: &TextLayer, written: &[u8]) -> Option<Vec<E>> {
    let total = layer.text.encode_utf16().count().checked_add(1)?;
    let mut ed: Vec<(usize, Vec<(String, E)>)> = Vec::new();
    let mut left = total;
    for (len, attrs) in ed_runs(written, RunKind::Style)? {
        let take = len.min(left);
        if take > 0 {
            ed.push((take, attrs));
            left -= take;
        }
    }
    match ed.last_mut() {
        Some(last) => last.0 += left,
        None => ed.push((total, Vec::new())),
    }
    let modes = txt2_mode_runs(layer);
    let (mut mi, mut mode_left) = (0usize, modes.first().map_or(usize::MAX, |m| m.1));
    let mut out = Vec::new();
    let mut at = 0usize;
    for (len, attrs) in ed {
        let mut left = len;
        while left > 0 {
            while mode_left == 0 && mi + 1 < modes.len() {
                mi += 1;
                mode_left = modes.get(mi).map_or(usize::MAX, |m| m.1);
            }
            let piece = if mode_left == 0 { left } else { left.min(mode_left) };
            let inner = dict(vec![("0", E::String(String::new())), ("5", E::Int(1)), ("6", parsed(RUN_STYLE)?)]);
            let mut run = dict(vec![("0", dict(vec![("0", inner)])), ("1", int(piece))]);
            for (key, v) in &attrs {
                // An attribute `Txt2` has no key for keeps the default's value.
                let _ = set_txt2_attr(txt2, &mut run, RunKind::Style, key, v);
            }
            let style = run.get_mut("0")?.get_mut("0")?.get_mut("6")?;
            set_style_extras(style, &style_at(layer, at), modes.get(mi).map_or(1, |m| m.0));
            out.push(run);
            left -= piece;
            mode_left = mode_left.saturating_sub(piece);
            at = at.checked_add(piece)?;
        }
    }
    Some(out)
}

/// The `Txt2` paragraph runs of the layer written as `written`: one per paragraph (`lens`, UTF-16
/// lengths with each break), each [`RUN_PARAGRAPH`] with the attributes of the EngineData
/// paragraph run it starts in.
fn paragraph_runs(txt2: &E, written: &[u8], lens: &[usize]) -> Option<Vec<E>> {
    let runs = ed_runs(written, RunKind::Paragraph)?;
    let mut out = Vec::with_capacity(lens.len());
    let mut at = 0usize;
    for len in lens {
        let mut end = 0usize;
        let attrs = runs
            .iter()
            .find(|(n, _)| {
                end = end.saturating_add(*n);
                at < end
            })
            .or(runs.last())
            .map(|r| r.1.clone())
            .unwrap_or_default();
        let inner = dict(vec![("0", E::String(String::new())), ("5", parsed(RUN_PARAGRAPH)?), ("6", E::Int(1))]);
        let mut run = dict(vec![("0", dict(vec![("0", inner)])), ("1", int(*len))]);
        // EngineData's `ConsecutiveHyphens` 8 is `Txt2`'s 0 in every Reference file (56 of the
        // oracle corpus): the template's value stands.
        for (key, v) in attrs.iter().filter(|(k, _)| k != "ConsecutiveHyphens") {
            let _ = set_txt2_attr(txt2, &mut run, RunKind::Paragraph, key, v);
        }
        if let Some(every) = attrs.iter().find(|a| a.0 == "EveryLineComposer").and_then(|a| a.1.as_bool()) {
            run.get_mut("0")?.get_mut("0")?.get_mut("5")?.set("29", E::Bool(every));
        }
        at = at.checked_add(*len)?;
        out.push(run);
    }
    Some(out)
}

/// The frame (`/0 /8 /0` entry) of a layer: Reference's point stub, or a box outlining the
/// layer's box (in text space, as the `TySh` holds it); vertical type's frames say so (`/2 /1`).
fn frame_for(layer: &TextLayer, written: &[u8]) -> Option<E> {
    let boxed = matches!(layer.shape, TextShape::Box { .. }) && layer.path.is_none();
    let mut frame = new_frame(boxed);
    if let TextShape::Box { x, y, width, height } = layer.shape
        && boxed
    {
        let shape = [x, y, x + width, y + height].map(f64::from);
        frame.get_mut("0")?.get_mut("1")?.set("0", box_frame_points(box_bounds(written).unwrap_or(shape)));
    }
    if layer.orientation == Orientation::Vertical
        && let Some(E::Dict(settings)) = frame.get_mut("0").and_then(|f| f.get_mut("2"))
    {
        let at = usize::from(settings.first().is_some_and(|(k, _)| k == "0"));
        settings.insert(at, ("1".into(), E::Int(2)));
    }
    Some(frame)
}

/// The text object of the layer written as `written` (its text object `index`, drawn in frame
/// `index`): the model (text, paragraph and style runs, the anti-aliasing when not Smooth) and
/// the view (the engine version, its frame, one line per paragraph and an empty layout).
fn text_object(txt2: &E, layer: &TextLayer, written: &[u8], index: usize) -> Option<E> {
    let text = format!("{}\r", layer.text.replace('\n', "\r"));
    let lens: Vec<usize> = text.split_inclusive('\r').map(|p| p.encode_utf16().count()).collect();
    let mut model = vec![
        ("0", E::String(text)),
        ("5", dict(vec![("0", E::Array(paragraph_runs(txt2, written, &lens)?))])),
        ("6", dict(vec![("0", E::Array(style_runs(txt2, layer, written)?))])),
    ];
    // EngineData's numbering of the modes; Smooth (Reference's default) is written as no entry.
    let aa = match layer.antialias {
        AntiAlias::None => Some(0),
        AntiAlias::Crisp => Some(1),
        AntiAlias::Strong => Some(2),
        AntiAlias::Smooth => None,
        AntiAlias::Sharp => Some(4),
        AntiAlias::Windows => Some(5),
        AntiAlias::WindowsLcd => Some(6),
    };
    if let Some(aa) = aa {
        model.push(("10", dict(vec![("0", E::Int(aa)), ("2", E::Bool(true))])));
    }
    let lines = lens.iter().map(|len| dict(vec![("0", dict(vec![("1", E::Int(1))])), ("1", int(*len))])).collect();
    let frame_strike = dict(vec![("99", E::Name("F".into())), ("10", int(index)), ("5", E::Int(2)), ("6", E::Array(Vec::new()))]);
    let layout = dict(vec![("99", E::Name("PC".into())), ("5", E::Int(0)), ("6", E::Array(vec![frame_strike]))]);
    let view = dict(vec![
        ("4", engine_version()),
        ("5", E::Int(0)),
        ("0", E::Array(vec![dict(vec![("0", int(index))])])),
        ("1", dict(vec![("0", E::Array(lines))])),
        ("2", E::Array(vec![layout])),
    ]);
    Some(dict(vec![("0", dict(model)), ("1", view)]))
}

/// The document settings (`/1 /0`): hidden characters drawn in font `hidden`, the normal sheets,
/// the superscript, subscript and small caps scales, the smart quotes.
fn document_settings(hidden: usize) -> E {
    let whitespace = WHITESPACE.iter().map(|(c, g)| dict(vec![("0", E::String((*c).into())), ("1", E::String((*g).into()))])).collect();
    let quotes = SMART_QUOTES
        .iter()
        .enumerate()
        .map(|(lang, q)| {
            let mut entry = vec![("0", int(lang))];
            for (key, s) in ["1", "2", "3", "4"].into_iter().zip(q) {
                if !s.is_empty() {
                    entry.push((key, E::String((*s).into())));
                }
            }
            dict(entry)
        })
        .collect();
    dict(vec![
        ("0", dict(vec![("0", int(hidden)), ("1", E::Array(whitespace))])),
        ("1", E::Int(0)),
        ("2", E::Int(0)),
        ("3", E::Real(0.583)),
        ("4", E::Real(0.333)),
        ("5", E::Real(0.583)),
        ("6", E::Real(0.333)),
        ("7", E::Real(0.7)),
        ("8", E::Bool(true)),
        ("9", E::Array(quotes)),
        ("15", dict(vec![("0", E::String(String::new()))])),
        ("16", E::Bool(true)),
        ("17", E::Int(0)),
    ])
}

/// The document resources but the font list and frames (`/0 /2` to `/0 /6`, `/0 /9`), in
/// Reference's order, the default fonts at `normal`.
fn shared_resources(normal: usize) -> Option<Vec<(&'static str, E)>> {
    let name = |s: &str| E::String(s.into());
    let code_to_class = resource_set(vec![dict(vec![("0", dict(vec![("0", name(""))]))])], &[0]);
    let mojikumi = resource_set(
        MOJIKUMI.iter().map(|(n, class)| dict(vec![("0", dict(vec![("0", name(n)), ("5", dict(vec![("0", E::Int(0)), ("3", E::Int(*class))]))]))])).collect(),
        &(0..MOJIKUMI.len()).collect::<Vec<_>>(),
    );
    let kinsoku_entry = |n: &str, chars: [&str; 4], kind: i64| {
        let mut set: Vec<(&str, E)> = ["0", "1", "2", "3"].into_iter().zip(chars).map(|(k, c)| (k, name(c))).collect();
        set.push(("4", E::Int(kind)));
        dict(vec![("0", dict(vec![("0", name(n)), ("5", dict(set))]))])
    };
    let kinsoku = resource_set(
        vec![
            kinsoku_entry("None", ["", "", "", ""], 0),
            kinsoku_entry("PhotoshopKinsokuHard", KINSOKU_HARD, 1),
            kinsoku_entry("PhotoshopKinsokuSoft", KINSOKU_SOFT, 2),
            kinsoku_entry("Hard", KINSOKU_HARD, 1),
            kinsoku_entry("Soft", KINSOKU_SOFT, 2),
            kinsoku_entry("Soft_v2", KINSOKU_SOFT_V2, 3),
        ],
        &[0, 1, 2, 3, 4, 5],
    );
    let mut normal_style = parsed(NORMAL_STYLE)?;
    normal_style.set("0", int(normal));
    let styles = resource_set(
        vec![
            dict(vec![("0", dict(vec![("0", name("Normal RGB")), ("6", normal_style)]))]),
            dict(vec![("0", dict(vec![("0", name("None")), ("5", E::Int(0)), ("6", E::dict())]))]),
        ],
        &[0, 1],
    );
    let lists = resource_set(
        LIST_STYLES.iter().map(|(n, tag)| dict(vec![("0", dict(vec![("0", name(n)), ("6", E::Int(*tag))]))])).collect(),
        &(0..LIST_STYLES.len()).collect::<Vec<_>>(),
    );
    let mut basic = parsed(NORMAL_PARAGRAPH)?;
    basic.set("9", E::Bool(false));
    let mut basic_style = parsed(BASIC_PARAGRAPH_STYLE)?;
    basic_style.set("0", int(normal));
    basic.set("32", basic_style);
    let paragraphs = resource_set(
        vec![
            dict(vec![("0", dict(vec![("0", name("Normal RGB")), ("5", parsed(NORMAL_PARAGRAPH)?)]))]),
            dict(vec![("0", dict(vec![("0", name("Basic Paragraph")), ("5", basic), ("6", E::Int(0))]))]),
        ],
        &[1, 0],
    );
    Some(vec![("2", code_to_class), ("3", mojikumi), ("4", kinsoku), ("5", styles), ("9", lists), ("6", paragraphs)])
}

/// The document's `Txt2` (parsed) for `objects`, the type layers as written: `(layer, TySh)`, the
/// i-th written with `TextIndex` i (else `None`). `previous` is the document's earlier block, if
/// any, whose font entries are kept for the faces it lists; `details` describes any other face
/// (its version string and, for a variable font's instance, its axis values; see
/// [`crate::FontDb::txt2_font_details`]), given the style naming it and its PostScript name.
/// Sizes and lengths come from the `TySh`s, already in text-space units. `None` when a layer's
/// `TySh` doesn't parse, or there are no layers or more than [`super::MAX_TEXT_INDEX`] + 1.
pub fn new_txt2(objects: &[(&TextLayer, &[u8])], previous: Option<&E>, details: &mut dyn FnMut(&CharStyle, &str) -> FontDetails) -> Option<E> {
    if objects.is_empty() || objects.len() > super::MAX_TEXT_INDEX as usize + 1 {
        return None;
    }
    for (i, (_, written)) in objects.iter().enumerate() {
        if text_index(written) != i64::try_from(i).ok() {
            return None;
        }
    }
    let (fonts, normal, hidden) = font_list(objects, previous, details)?;
    // The faces' versions, as the font-use record (`/1 /5`) lists them.
    let versions: Vec<E> = fonts
        .iter()
        .enumerate()
        .filter_map(|(i, f)| f.path(&["0", "0", "5"]).filter(|_| i != normal && i != hidden).map(|v| dict(vec![("0", int(i)), ("1", v.clone())])))
        .collect();
    let lookup = dict(vec![("0", dict(vec![("1", dict(vec![("0", E::Array(fonts.clone()))]))]))]);
    let mut texts = Vec::with_capacity(objects.len());
    let mut frames = Vec::with_capacity(objects.len());
    for (i, (layer, written)) in objects.iter().enumerate() {
        texts.push(text_object(&lookup, layer, written, i)?);
        frames.push(frame_for(layer, written)?);
    }
    let mut resources = vec![("1", dict(vec![("0", E::Array(fonts))]))];
    resources.extend(shared_resources(normal)?);
    resources.push(("8", dict(vec![("0", E::Array(frames))])));
    let mut normal_style = parsed(NORMAL_STYLE)?;
    normal_style.set("0", int(normal));
    let record = dict(vec![
        ("0", E::Int(0)),
        ("1", engine_version()),
        ("2", E::Bytes(FONT_RECORD_FLAGS.0.as_bytes().to_vec())),
        ("3", E::Bytes(FONT_RECORD_FLAGS.1.as_bytes().to_vec())),
        ("4", E::Array(versions)),
    ]);
    let document = dict(vec![
        ("4", engine_version()),
        ("0", document_settings(hidden)),
        ("5", E::Array(vec![record])),
        ("1", E::Array(texts)),
        ("2", normal_style),
        ("3", parsed(NORMAL_PARAGRAPH)?),
    ]);
    Some(dict(vec![("98", dict(vec![("0", E::Int(14))])), ("0", dict(resources)), ("1", document)]))
}

/// Gives the font list entries of `txt2` from `from` on (the ones a save added) the axis values
/// and version string `details` knows for the faces they name, where they have none: the style
/// of `layers` naming each face is asked (see [`new_txt2`]). Entries Reference wrote, before
/// `from`, are left as they are.
pub fn fill_txt2_fonts(txt2: &mut E, from: usize, layers: &[&TextLayer], details: &mut dyn FnMut(&CharStyle, &str) -> FontDetails) {
    let Some(E::Array(list)) = txt2.get_mut("0").and_then(|v| v.get_mut("1")).and_then(|v| v.get_mut("0")) else { return };
    for entry in list.iter_mut().skip(from) {
        let Some(font @ E::Dict(_)) = entry.get_mut("0").and_then(|v| v.get_mut("0")) else { continue };
        let Some(name) = font.get("0").and_then(E::as_str).map(str::to_string) else { continue };
        if font.get("4").is_some() && font.get("5").is_some() {
            continue;
        }
        let Some(style) = layers.iter().flat_map(|l| l.char_runs()).map(|r| r.style).find(|s| postscript_for(s) == name) else { continue };
        let d = details(&style, &name);
        let E::Dict(items) = font else { continue };
        let mut put = |key: &str, v: E| {
            if items.iter().any(|(k, _)| k == key) {
                return;
            }
            // Keys in Reference's order (`/0` … `/5`).
            let at = items.iter().position(|(k, _)| k.parse::<u32>().is_ok_and(|n| key.parse::<u32>().is_ok_and(|m| n > m))).unwrap_or(items.len());
            items.insert(at, (key.to_string(), v));
        };
        if !d.axes.is_empty() {
            put("4", axis_values(&d.axes));
        }
        if let Some(v) = d.version.filter(|v| !v.is_empty()) {
            put("5", E::String(v));
        }
    }
}

#[cfg(test)]
#[path = "fresh_tests.rs"]
mod tests;
