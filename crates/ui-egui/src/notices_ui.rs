//! Help › Third-Party Notices: the attribution and licence notices shipped with PhotoSuite, in
//! two tabs: `THIRD-PARTY-NOTICES.md` (fonts, icons, presets, data) and `THIRD-PARTY-CRATES.md`
//! (the Rust crates, with their licence texts). Both are compiled in, so every build, the web
//! one too, carries them where a recipient of the binary can read them.
//!
//! The files are rendered from the small Markdown subset they use: headings, tables, lists,
//! code blocks, rules, links, bold and code spans. Lines keep the files' own wrapping, and the
//! crates file runs to some ten thousand lines, so each line gets a fixed height up front and
//! only the lines in view are laid out.

use std::sync::OnceLock;

use egui::{Color32, FontId, Pos2, Rect, Sense, Stroke, vec2};
use serde_json::{Map, Value, json};

use crate::theme::Tokens;

const NOTICES: &str = include_str!("../../../THIRD-PARTY-NOTICES.md");
const CRATES: &str = include_str!("../../../THIRD-PARTY-CRATES.md");

/// The dialog's width, and its scrolling body's height.
pub const WIDTH: f32 = 760.0;
const BODY_H: f32 = 520.0;

/// The field that marks an About dialog as this one.
pub const FIELD: &str = "notices";

/// Whether the About dialog's fields are this dialog's.
pub fn owns(f: &Map<String, Value>) -> bool {
    f.get(FIELD).and_then(Value::as_bool) == Some(true)
}

/// Fields that open the dialog.
pub fn fields() -> Map<String, Value> {
    let mut f = Map::new();
    f.insert(FIELD.into(), json!(true));
    f
}

#[derive(Debug, Clone, PartialEq)]
struct Span {
    text: String,
    /// Only web links: the files' relative links name files in the source tree.
    url: Option<String>,
    code: bool,
    bold: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Line {
    Heading(usize, Vec<Span>),
    Text(Vec<Span>),
    Bullet(Vec<Span>),
    /// A table row: its cells, whether it is the header, and which table (for column widths).
    Row(Vec<Vec<Span>>, bool, usize),
    Code(String),
    Rule,
    Gap,
}

impl Line {
    fn height(&self) -> f32 {
        match self {
            Line::Heading(1, _) => 34.0,
            Line::Heading(2, _) => 30.0,
            Line::Heading(..) => 25.0,
            Line::Text(_) | Line::Bullet(_) => 18.0,
            Line::Row(..) => 21.0,
            Line::Code(_) => 15.0,
            Line::Rule => 15.0,
            Line::Gap => 8.0,
        }
    }
}

/// A parsed file: its lines, each line's top, the total height, and each table's column widths.
struct Doc {
    lines: Vec<Line>,
    tops: Vec<f32>,
    height: f32,
    columns: Vec<Vec<f32>>,
}

/// Inline spans: `[label](url)`, `**bold**`, `` `code` ``, and plain text.
fn inline(s: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut rest = s;
    let flush = |plain: &mut String, out: &mut Vec<Span>| {
        if !plain.is_empty() {
            out.push(Span { text: std::mem::take(plain), url: None, code: false, bold: false });
        }
    };
    while let Some(c) = rest.chars().next() {
        if c == '['
            && let Some(close) = rest.find("](")
            && let Some(label) = rest.get(1..close)
            && let Some(after) = rest.get(close + 2..)
            && let Some(end) = after.find(')')
        {
            flush(&mut plain, &mut out);
            let url = after.get(..end).unwrap_or_default();
            let (text, code) = match label.strip_prefix('`').and_then(|l| l.strip_suffix('`')) {
                Some(inner) => (inner, true),
                None => (label, false),
            };
            let web = url.starts_with("https://") || url.starts_with("http://");
            out.push(Span { text: text.to_string(), url: web.then(|| url.to_string()), code, bold: false });
            rest = after.get(end + 1..).unwrap_or_default();
            continue;
        }
        if let Some(body) = rest.strip_prefix("**")
            && let Some(end) = body.find("**")
        {
            flush(&mut plain, &mut out);
            out.push(Span { text: body.get(..end).unwrap_or_default().to_string(), url: None, code: false, bold: true });
            rest = body.get(end + 2..).unwrap_or_default();
            continue;
        }
        if let Some(body) = rest.strip_prefix('`')
            && let Some(end) = body.find('`')
        {
            flush(&mut plain, &mut out);
            out.push(Span { text: body.get(..end).unwrap_or_default().to_string(), url: None, code: true, bold: false });
            rest = body.get(end + 1..).unwrap_or_default();
            continue;
        }
        plain.push(c);
        rest = rest.get(c.len_utf8()..).unwrap_or_default();
    }
    flush(&mut plain, &mut out);
    out
}

/// A rough width for column sizing (measuring every cell's text would mean laying out every
/// row of the crates table up front).
fn estimate(spans: &[Span]) -> f32 {
    spans.iter().map(|s| s.text.chars().count() as f32 * if s.code { 7.2 } else { 6.6 }).sum()
}

fn parse(text: &str) -> Doc {
    let mut lines = Vec::new();
    let mut columns: Vec<Vec<f32>> = Vec::new();
    let mut in_code = false;
    let mut in_table = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            lines.push(Line::Code(raw.replace('\t', "    ").trim_end().to_string()));
            continue;
        }
        if !line.starts_with('|') {
            in_table = false;
        }
        if line.starts_with("<!--") {
            continue;
        }
        let next = if line.is_empty() {
            Line::Gap
        } else if line == "---" || line == "***" {
            Line::Rule
        } else if let Some((hashes, title)) = line.split_once(' ').filter(|(h, _)| (1..=4).contains(&h.len()) && h.chars().all(|c| c == '#')) {
            Line::Heading(hashes.len(), inline(title))
        } else if line.starts_with('|') {
            let inner = line.trim_start_matches('|');
            let inner = inner.strip_suffix('|').unwrap_or(inner);
            // The dashed row only marks the header above it.
            if inner.chars().all(|c| matches!(c, '-' | ':' | '|' | ' ')) {
                continue;
            }
            let cells: Vec<Vec<Span>> = inner.split('|').map(|c| inline(c.trim())).collect();
            let header = !in_table;
            if header {
                columns.push(Vec::new());
            }
            in_table = true;
            let table = columns.len().saturating_sub(1);
            if let Some(widths) = columns.last_mut() {
                for (i, c) in cells.iter().enumerate() {
                    let w = (estimate(c) + 20.0).min(380.0);
                    match widths.get_mut(i) {
                        Some(old) => *old = old.max(w),
                        None => widths.push(w),
                    }
                }
            }
            Line::Row(cells, header, table)
        } else if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            Line::Bullet(inline(item))
        } else {
            Line::Text(inline(line))
        };
        // One gap between blocks, however many blank lines.
        if next == Line::Gap && matches!(lines.last(), None | Some(Line::Gap)) {
            continue;
        }
        lines.push(next);
    }
    let mut tops = Vec::with_capacity(lines.len());
    let mut y = 0.0;
    for l in &lines {
        tops.push(y);
        y += l.height();
    }
    Doc { lines, tops, height: y, columns }
}

fn docs() -> &'static [Doc; 2] {
    static DOCS: OnceLock<[Doc; 2]> = OnceLock::new();
    DOCS.get_or_init(|| [parse(NOTICES), parse(CRATES)])
}

/// Paints `spans` from `at` (left, vertical centre), clipped to `clip`. Returns the web link
/// clicked, if any.
fn spans(ui: &egui::Ui, painter: &egui::Painter, id: egui::Id, at: Pos2, spans: &[Span], size: f32, color: Color32) -> Option<String> {
    let t = Tokens::get(ui.ctx());
    let mut x = at.x;
    let mut clicked = None;
    for (k, s) in spans.iter().enumerate() {
        let font = if s.code {
            crate::theme::mono(size - 1.0)
        } else if s.bold {
            crate::theme::semibold(size)
        } else {
            FontId::proportional(size)
        };
        let col = if s.url.is_some() {
            t.accent
        } else if s.code {
            t.text_dim
        } else {
            color
        };
        let galley = painter.layout_no_wrap(s.text.clone(), font, col);
        let r = Rect::from_min_size(Pos2::new(x, at.y - galley.size().y / 2.0), galley.size());
        if s.code {
            painter.rect_filled(r.expand2(vec2(3.0, 1.0)), 3.0, t.field);
        }
        if let Some(url) = &s.url {
            let resp = ui.interact(r.intersect(painter.clip_rect()), id.with(k), Sense::click()).on_hover_text(url);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                painter.line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, col));
            }
            if resp.clicked() {
                clicked = Some(url.clone());
            }
        }
        painter.galley(r.min, galley, col);
        x = r.right() + if s.code { 6.0 } else { 0.0 };
    }
    clicked
}

/// The dialog body: the tabs and the selected file. Returns the web link clicked, if any.
pub fn body(ui: &mut egui::Ui, f: &mut Map<String, Value>) -> Option<String> {
    let t = Tokens::get(ui.ctx());
    ui.set_width(WIDTH);
    ui.spacing_mut().item_spacing.y = 0.0;
    let names = [tl!("Notices"), tl!("Rust Crates")];
    let mut tab = f.get("__tab").and_then(Value::as_u64).map_or(0, |v| v.min(1) as usize);
    let (strip, _) = ui.allocate_exact_size(vec2(WIDTH, 28.0), Sense::hover());
    ui.painter().rect_filled(strip, egui::CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 }, t.tab_strip);
    crate::tab_strip::pro_tabs(ui, ui.id().with("notices-tabs"), strip, strip.right(), &names, &mut tab, false);
    f.insert("__tab".into(), json!(tab));
    let doc = docs().get(tab)?;
    let mut clicked = None;
    egui::Frame::NONE.fill(t.card).inner_margin(egui::Margin::symmetric(16, 10)).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(("notices-body", tab)).max_height(BODY_H).auto_shrink([false, false]).show_viewport(ui, |ui, view| {
            let width = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(vec2(width, doc.height), Sense::hover());
            let painter = ui.painter_at(rect);
            let first = doc.tops.partition_point(|y| *y + 40.0 < view.min.y);
            for (i, line) in doc.lines.iter().enumerate().skip(first) {
                let Some(&top) = doc.tops.get(i) else { break };
                if top > view.max.y {
                    break;
                }
                let r = Rect::from_min_size(rect.min + vec2(0.0, top), vec2(width, line.height()));
                let id = ui.id().with(("notice", tab, i));
                let mid = Pos2::new(r.left(), r.center().y);
                let hit = match line {
                    Line::Heading(level, s) => {
                        let size = match level {
                            1 => 19.0,
                            2 => 15.5,
                            _ => 13.5,
                        };
                        let at = Pos2::new(r.left(), r.bottom() - size * 0.75);
                        let bold: Vec<Span> = s.iter().map(|s| Span { bold: true, ..s.clone() }).collect();
                        spans(ui, &painter, id, at, &bold, size, t.text)
                    }
                    Line::Text(s) => spans(ui, &painter, id, mid, s, 12.5, t.text_dim),
                    Line::Bullet(s) => {
                        painter.circle_filled(mid + vec2(5.0, 0.0), 2.0, t.text_faint);
                        spans(ui, &painter, id, mid + vec2(16.0, 0.0), s, 12.5, t.text_dim)
                    }
                    Line::Row(cells, header, table) => {
                        let widths = doc.columns.get(*table).map(Vec::as_slice).unwrap_or_default();
                        let mut x = r.left();
                        let mut hit = None;
                        for (c, cell) in cells.iter().enumerate() {
                            let w = widths.get(c).copied().unwrap_or(120.0);
                            let clip = Rect::from_min_max(Pos2::new(x, r.top()), Pos2::new((x + w - 8.0).min(r.right()), r.bottom()));
                            let cp = painter.with_clip_rect(clip.intersect(painter.clip_rect()));
                            let cell: Vec<Span> = cell.iter().map(|s| Span { bold: *header, ..s.clone() }).collect();
                            let color = if *header { t.text } else { t.text_dim };
                            hit = hit.or(spans(ui, &cp, id.with(c), Pos2::new(x + 6.0, r.center().y), &cell, 12.0, color));
                            x += w;
                        }
                        painter.line_segment(
                            [r.left_bottom(), r.right_bottom()],
                            Stroke::new(1.0, if *header { t.separator } else { t.separator.gamma_multiply(0.5) }),
                        );
                        hit
                    }
                    Line::Code(text) => {
                        painter.rect_filled(r, 0.0, t.field);
                        painter.text(mid + vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, text, crate::theme::mono(11.0), t.text_dim);
                        None
                    }
                    Line::Rule => {
                        painter.line_segment([mid, Pos2::new(r.right(), mid.y)], Stroke::new(1.0, t.separator));
                        None
                    }
                    Line::Gap => None,
                };
                if clicked.is_none() {
                    clicked = hit;
                }
            }
        });
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_files_parse_into_their_sections() {
        let [notices, crates] = docs();
        let headings = |d: &Doc| d.lines.iter().filter(|l| matches!(l, Line::Heading(..))).count();
        assert!(headings(notices) > 10 && headings(crates) > 10);
        // Every crate in the table is a row (plus the header), and its name links to crates.io.
        let rows: Vec<&Vec<Vec<Span>>> = crates.lines.iter().filter_map(|l| if let Line::Row(c, false, 0) = l { Some(c) } else { None }).collect();
        let stated = CRATES.lines().find_map(|l| l.strip_prefix("## Crates (")?.strip_suffix(')')?.parse::<usize>().ok()).unwrap();
        assert_eq!(rows.len(), stated);
        assert!(rows.iter().all(|c| c[0][0].url.as_deref().is_some_and(|u| u.starts_with("https://crates.io/crates/"))));
        // The licence texts are code lines, not prose.
        assert!(crates.lines.iter().filter(|l| matches!(l, Line::Code(_))).count() > 5000);
        assert_eq!(crates.tops.len(), crates.lines.len());
    }

    #[test]
    fn inline_spans_keep_text_and_only_web_links() {
        let s = inline("see [`LICENSE-MIT`](LICENSE-MIT) and [Inter](https://github.com/rsms/inter), **bold** `x` é");
        let text: String = s.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "see LICENSE-MIT and Inter, bold x é");
        assert_eq!(s[1], Span { text: "LICENSE-MIT".into(), url: None, code: true, bold: false });
        assert_eq!(s[3].url.as_deref(), Some("https://github.com/rsms/inter"));
        assert!(s[5].bold && s[7].code);
        // Unclosed markup is plain text, never a panic.
        for odd in ["[", "[a](", "[a](b", "**", "`", "](x)", "é[`", "|", "# ", "####### x"] {
            let _ = inline(odd);
            let _ = parse(odd);
        }
    }

    #[test]
    fn body_renders_both_tabs() {
        let ctx = egui::Context::default();
        crate::PhotosuiteApp::setup_context(&ctx, crate::theme::ThemeKind::ALL[0]);
        let mut f = fields();
        for tab in [0, 1, 7] {
            f.insert("__tab".into(), json!(tab));
            let mut out = ctx.run_ui(Default::default(), |ui| {
                let _ = body(ui, &mut f);
            });
            out.textures_delta.clear();
        }
        assert_eq!(f["__tab"], json!(1), "an out-of-range tab is the last one");
    }
}
