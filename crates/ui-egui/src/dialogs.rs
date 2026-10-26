//! Dialogs rendered from `UiState::dialogs`. Field values live in the dialog data, so automation can
//! set them (`ui.dialog.set`) and confirm (`ui.dialog.confirm`) exactly like a user.

use serde_json::{Value, json};

use crate::PhotospliceApp;
use crate::state::{Dialog, DialogKind};
use crate::widgets::{ButtonRole, DialogButton, dialog_buttons};

/// Where this frame's dialogs are on screen (the canvas reads last frame's: it draws first).
const RECTS: &str = "pc-dialog-rects";

/// Room under a dialog's content beyond the frame's margin, so its button row doesn't sit on the
/// bottom edge.
const BOTTOM_PAD: i8 = 8;

fn rects(ctx: &egui::Context) -> Vec<egui::Rect> {
    ctx.data(|m| m.get_temp(egui::Id::new(RECTS))).unwrap_or_default()
}

/// With a dialog open the rest of the window is inert (egui's modal layer), but like Photoshop the
/// image can still be panned and zoomed under it. The pointer position when it is over `canvas`
/// and not over a dialog or one of its popups.
pub fn free_pointer_over(ctx: &egui::Context, canvas: egui::Rect) -> Option<egui::Pos2> {
    if egui::Popup::is_any_open(ctx) {
        return None;
    }
    let p = ctx.pointer_hover_pos()?;
    let rects = rects(ctx);
    (canvas.contains(p) && !rects.iter().any(|r| r.contains(p))).then_some(p)
}

/// Pan drag under an open dialog: Space-drag, middle-drag, or a drag with the Hand tool, started on
/// the free canvas. Returns this frame's pointer movement.
pub fn pan_delta(ctx: &egui::Context, canvas: egui::Rect, hand: bool) -> Option<egui::Vec2> {
    let rects = rects(ctx);
    let (origin, panning, delta) = ctx.input(|i| {
        let p = &i.pointer;
        (p.press_origin(), p.middle_down() || (p.primary_down() && (hand || i.key_down(egui::Key::Space))), p.delta())
    });
    let origin = origin?;
    (panning && canvas.contains(origin) && !rects.iter().any(|r| r.contains(origin))).then_some(delta)
}

/// How a dialog's frame is sized: a width range for the content to fill (the ordinary
/// dialogs), or a fixed outer size.
#[derive(Clone, Copy, Debug)]
pub enum FrameSize {
    Width(f32, f32),
    /// A fixed outer size, the body inside the frame's margins (Import PDF).
    Fixed(egui::Vec2),
    /// A fixed outer size whose body paints its own panels and footer (Camera Raw, Filter
    /// Gallery, Lens Correction, Liquify): in a window of its own they run to the window's
    /// edges, in the main window to an even gutter all round. The body pads its own content.
    Panels(egui::Vec2),
}

impl FrameSize {
    fn outer(self) -> Option<egui::Vec2> {
        match self {
            Self::Width(..) => None,
            Self::Fixed(outer) | Self::Panels(outer) => Some(outer),
        }
    }
}

/// What the dialog loop ([`show`]) adds to a [`frame`].
#[derive(Clone, Copy, Default)]
struct FrameOpts {
    /// Where the dialog's place is remembered between runs (egui's persisted memory): a dialog the
    /// user moved reopens there, as Photoshop remembers each dialog's place.
    place: Option<egui::Id>,
    /// Greyed out, the whole frame: only the topmost dialog takes input.
    inactive: bool,
    /// Where it opens the first time instead of centred (the Blur Gallery's options sit at the
    /// right, clear of the pins on the image).
    first: Option<egui::Vec2>,
}

/// Every dialog's frame: a modal over the document (not dimmed: Photoshop doesn't, so previews
/// are judged at true contrast), a title bar in the window title bar's colour, dragged by that
/// title bar and kept on screen below the app's title bar (on macOS the system's: a dialog there
/// would sit under the traffic lights, and a drag would move the window).
///
/// `body` gets the frame's content `Ui` and, for a fixed size, the body's size below the title
/// bar. The response says whether it should close (Esc is the caller's).
///
/// A large dialog ([`FrameSize::Fixed`], [`FrameSize::Panels`]: Camera Raw, Filter Gallery…) is
/// a window of its own where the platform has them ([`floating`]): it can leave the main window,
/// as Reference's large dialogs do. The small ones stay in the main window.
pub fn frame<R>(
    ctx: &egui::Context,
    id: egui::Id,
    title: &str,
    size: FrameSize,
    body: impl FnOnce(&mut egui::Ui, Option<egui::Vec2>) -> R,
) -> FrameResponse<R> {
    if let Some(outer) = size.outer()
        && !ctx.embed_viewports()
    {
        return floating(ctx, id, title, outer, matches!(size, FrameSize::Panels(_)), body);
    }
    let modal = frame_with(ctx, id, title, size, FrameOpts::default(), body);
    // Esc closes it; a click outside doesn't (Reference keeps the dialog).
    let close = modal.is_top_modal && !modal.any_popup_open && ctx.input(|i| i.key_pressed(egui::Key::Escape));
    FrameResponse { close, inner: Some(modal.inner) }
}

/// What [`frame`] returns: the body's value (`None` only if a separate window never drew it) and
/// whether the dialog asked to close (Esc with no popup open, or the close box of its own window).
pub struct FrameResponse<R> {
    pub inner: Option<R>,
    close: bool,
}

impl<R> FrameResponse<R> {
    pub fn should_close(&self) -> bool {
        self.close
    }
}

/// Key presses from a floating dialog's window, handed to the main window on the
/// next frame ([`forward_floating_keys`]): the dialogs read their keys (Esc, Enter, Liquify's tool
/// letters, Camera Raw's Y and ⌘Z) where the main window's shortcuts do, as when they were drawn
/// in it.
const FLOATING_KEYS: &str = "pc-floating-dialog-keys";

#[derive(Clone, Default)]
struct FloatingKeys {
    events: Vec<egui::Event>,
}

/// Where a floating dialog opens (screen points) and at what size, kept while it stays open so the
/// window isn't moved under the user each frame. `measured` is a content-sized window's last
/// measured size (none before its first frame, while it stays hidden).
#[derive(Clone, Copy)]
struct FloatingPlace {
    pos: egui::Pos2,
    size: egui::Vec2,
    measured: Option<egui::Vec2>,
    pass: u64,
}

/// How a floating dialog's window is sized.
#[derive(Clone, Copy)]
enum FloatSize {
    /// A given size the user can change (the large tool dialogs).
    Fixed(egui::Vec2),
    /// Its content's, in a width range (Layer Style, New Document, Preferences): the window
    /// follows the content.
    Content(f32, f32),
}

/// What the dialog loop adds to a floating window.
#[derive(Clone, Copy)]
struct FloatOpts {
    /// Where its place (relative to the main window) is remembered between runs.
    place: egui::Id,
    /// Greyed out: a dialog above it takes the input.
    inactive: bool,
    /// The window a press on the main window (or on an inactive dialog) brings to the front.
    front: egui::ViewportId,
    /// Hand its unused key presses to the main window ([`forward_floating_keys`]): the large tool
    /// dialogs read Esc, Enter and their shortcuts there. The dialog loop's dialogs read theirs
    /// in their own window.
    forward_keys: bool,
    /// The body runs to the window's edges ([`FrameSize::Panels`]) instead of inside a margin.
    bleed: bool,
}

/// The size of the main window's content (points), also while drawing in a dialog's own window:
/// dialogs that fit themselves to the window (Preferences, New Document) fit to this one.
pub fn host_rect(ctx: &egui::Context) -> egui::Rect {
    ctx.data(|m| m.get_temp::<egui::Rect>(egui::Id::new(HOST_RECT))).unwrap_or_else(|| ctx.content_rect())
}

const HOST_RECT: &str = "pc-dialog-host-rect";

/// Records the main window's content rect for [`host_rect`]; call once per frame from it.
pub fn note_host_rect(ctx: &egui::Context) {
    let r = ctx.content_rect();
    ctx.data_mut(|m| m.insert_temp(egui::Id::new(HOST_RECT), r));
}

/// A large dialog in a window of its own, styled like the in-window frame: PhotoSplice's title
/// strip (on macOS in place of the system's title bar, beside the window buttons) over the dialog
/// background. The main window stays inert under it (not dimmed) and a press there brings the
/// dialog back to the front. Where it was left is remembered relative to the main window.
fn floating<R>(
    ctx: &egui::Context,
    id: egui::Id,
    title: &str,
    outer: egui::Vec2,
    bleed: bool,
    body: impl FnOnce(&mut egui::Ui, Option<egui::Vec2>) -> R,
) -> FrameResponse<R> {
    let opts = FloatOpts { place: id.with("floating-offset"), inactive: false, front: egui::ViewportId::from_hash_of(id), forward_keys: true, bleed };
    let shown = floating_window(ctx, id, title, FloatSize::Fixed(outer), opts, body);
    FrameResponse { inner: shown.inner, close: shown.close }
}

/// What [`floating_window`] returns.
struct Floated<R> {
    inner: Option<R>,
    /// Esc (no popup open) or the window's close box.
    close: bool,
}

fn floating_window<R>(
    ctx: &egui::Context,
    id: egui::Id,
    title: &str,
    sizing: FloatSize,
    opts: FloatOpts,
    body: impl FnOnce(&mut egui::Ui, Option<egui::Vec2>) -> R,
) -> Floated<R> {
    let t = crate::theme::Tokens::get(ctx);
    let vid = egui::ViewportId::from_hash_of(id);
    let main = ctx.content_rect();
    let pressed = egui::Area::new(id.with("floating-modal"))
        .order(egui::Order::Foreground)
        .fixed_pos(main.min)
        .show(ctx, |ui| ui.allocate_exact_size(main.size(), egui::Sense::click_and_drag()).1)
        .inner;
    if pressed.clicked() || pressed.drag_stopped() {
        ctx.send_viewport_cmd_to(opts.front, egui::ViewportCommand::Focus);
    }
    // Placed once when it opens (the builder is re-applied every frame, so it must not change).
    let main_outer = ctx.input(|i| i.viewport().outer_rect).unwrap_or(main);
    let place_key = id.with("floating-place");
    let pass = ctx.cumulative_pass_nr();
    let mut place = match ctx.data(|m| m.get_temp::<FloatingPlace>(place_key)) {
        Some(p) if p.pass.saturating_add(1) >= pass => p,
        _ => {
            let size = match sizing {
                FloatSize::Fixed(outer) => outer.max(egui::vec2(320.0, 240.0)),
                // As wide as the in-window frame lets it grow, until measured.
                FloatSize::Content(min_w, max_w) => egui::vec2(max_w.max(min_w).max(320.0) + 16.0, 400.0),
            };
            let centred = main_outer.center() - size / 2.0;
            // A remembered place is relative to the main window, and kept near it.
            let pos = ctx
                .data_mut(|m| m.get_persisted::<egui::Vec2>(opts.place))
                .filter(|o| o.is_finite() && o.x.abs() <= main_outer.width() && o.y.abs() <= main_outer.height())
                .map_or(centred, |o| main_outer.min + o);
            FloatingPlace { pos, size, measured: None, pass }
        }
    };
    place.pass = pass;
    let own_strip = cfg!(target_os = "macos");
    let strip_h = if own_strip { crate::panels::title_bar_height(ctx) } else { 0.0 };
    let mut builder = egui::ViewportBuilder::default().with_title(title).with_position(place.pos);
    builder = match sizing {
        FloatSize::Fixed(_) => builder
            .with_inner_size(place.size)
            .with_min_inner_size((place.size * 0.6).max(egui::vec2(480.0, 360.0)).min(place.size))
            .with_resizable(true),
        // Hidden until its content was measured once, then the content's size.
        FloatSize::Content(..) => builder.with_inner_size(place.measured.unwrap_or(place.size)).with_resizable(false).with_visible(place.measured.is_some()),
    };
    if own_strip {
        builder = builder.with_fullsize_content_view(true).with_titlebar_shown(false).with_title_shown(false);
    }
    let mut body = Some(body);
    let (mut close_box, mut escape, mut any_popup_open) = (false, false, false);
    let mut moved_to = None;
    let mut measured = None;
    let inner = ctx.show_viewport_immediate(vid, builder, |ui, _class| {
        let vctx = ui.ctx().clone();
        let (close_requested, outer_rect, pressed_here) =
            vctx.input(|i| (i.viewport().close_requested(), i.viewport().outer_rect, i.pointer.any_pressed()));
        close_box |= close_requested;
        moved_to = outer_rect.map(|r| r.min);
        if opts.inactive && pressed_here {
            vctx.send_viewport_cmd_to(opts.front, egui::ViewportCommand::Focus);
        }
        // The system title bar follows the theme where there is one (Windows, Linux).
        if !own_strip {
            let theme_key = id.with("floating-theme");
            let dark = t.dark();
            if vctx.data(|m| m.get_temp::<bool>(theme_key)) != Some(dark) {
                vctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(if dark { egui::SystemTheme::Dark } else { egui::SystemTheme::Light }));
                vctx.data_mut(|m| m.insert_temp(theme_key, dark));
            }
        }
        let out = egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(t.card))
            .show(ui, |ui| {
                if own_strip {
                    let (band, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), strip_h), egui::Sense::click_and_drag());
                    ui.painter().rect_filled(band, 0.0, t.chrome);
                    ui.painter().line_segment([band.left_bottom(), band.right_bottom()], egui::Stroke::new(1.0, t.separator));
                    let galley = ui.painter().layout_no_wrap(title.to_string(), crate::theme::semibold(13.0), t.text);
                    let at = egui::pos2(band.left() + 78.0, band.center().y - galley.size().y / 2.0);
                    ui.painter().with_clip_rect(band).galley(at, galley, t.text);
                    if resp.drag_started() {
                        vctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    // Panels start right under the strip's separator.
                    if opts.bleed {
                        ui.add_space(-ui.spacing().item_spacing.y);
                    }
                }
                if opts.inactive {
                    ui.disable();
                }
                let margin = if opts.bleed {
                    egui::Margin::ZERO
                } else {
                    let mut m = ui.spacing().menu_margin;
                    m.bottom = m.bottom.saturating_add(BOTTOM_PAD);
                    m
                };
                let content = egui::Frame::NONE.inner_margin(margin).show(ui, |ui| match sizing {
                    FloatSize::Fixed(_) => {
                        let size = ui.available_size();
                        body.take().map(|body| body(ui, Some(size)))
                    }
                    FloatSize::Content(min_w, max_w) => {
                        ui.set_min_width(min_w);
                        ui.set_max_width(max_w.max(min_w));
                        body.take().map(|body| body(ui, None))
                    }
                });
                if matches!(sizing, FloatSize::Content(..)) && !ui.is_sizing_pass() {
                    let r = content.response.rect;
                    measured = Some(egui::vec2(r.width(), strip_h + r.height()).round());
                }
                content.inner
            })
            .inner;
        any_popup_open = egui::Popup::is_any_open(&vctx);
        // Esc closes it, as in the in-window frame; it also goes on to the main window's handlers,
        // which some dialogs cancel from (Liquify, the Filter Gallery).
        escape = !any_popup_open && vctx.input(|i| i.key_pressed(egui::Key::Escape));
        // Unused key presses go to the main window's handlers; none while a text field has them.
        let (events, focused) =
            vctx.input(|i| (i.events.iter().filter(|e| matches!(e, egui::Event::Key { .. })).cloned().collect::<Vec<_>>(), i.viewport().focused));
        if opts.forward_keys && !opts.inactive && focused != Some(false) && !vctx.text_edit_focused() && !events.is_empty() {
            vctx.data_mut(|m| m.get_temp_mut_or_default::<FloatingKeys>(egui::Id::new(FLOATING_KEYS)).events.extend(events));
        }
        out
    });
    if let Some(size) = measured.filter(|s| s.is_finite() && s.x >= 1.0 && s.y >= 1.0) {
        if place.measured.is_none_or(|m| (m - size).length() > 0.5) {
            ctx.request_repaint();
        }
        place.measured = Some(size);
    }
    ctx.data_mut(|m| m.insert_temp(place_key, place));
    if let Some(at) = moved_to {
        let offset = at - main_outer.min;
        if offset.is_finite() {
            ctx.data_mut(|m| m.insert_persisted(opts.place, offset));
        }
    }
    Floated { inner, close: close_box || escape }
}

/// Hands a floating dialog's key presses ([`floating`]) to the main window's input:
/// call from the app's raw input hook.
pub fn forward_floating_keys(ctx: &egui::Context, raw: &mut egui::RawInput) {
    if raw.viewport_id != egui::ViewportId::ROOT {
        return;
    }
    let Some(keys) = ctx.data_mut(|m| m.remove_temp::<FloatingKeys>(egui::Id::new(FLOATING_KEYS))) else { return };
    raw.events.extend(keys.events);
}

/// [`frame`] with the dialog loop's options. A dialog opens centred below the title bar (or where
/// it was last left); once laid out its top-left stays put, so a dialog whose body grows (Layer
/// Style switching effects) extends down and right instead of re-centring.
fn frame_with<R>(
    ctx: &egui::Context,
    id: egui::Id,
    title: &str,
    size: FrameSize,
    opts: FrameOpts,
    body: impl FnOnce(&mut egui::Ui, Option<egui::Vec2>) -> R,
) -> egui::ModalResponse<R> {
    let bounds = crate::widgets::dialog_bounds(ctx);
    let screen = ctx.content_rect();
    // The top-left's offset from the window's (view state only, so egui memory).
    let pinned: Option<egui::Vec2> = ctx.data(|m| m.get_temp(id));
    let start = pinned.or_else(|| opts.place.and_then(|place| ctx.data_mut(|m| m.get_persisted::<egui::Vec2>(place)))).or(opts.first);
    let area = match start {
        Some(offset) if offset.is_finite() => egui::Modal::default_area(id).anchor(egui::Align2::LEFT_TOP, offset),
        _ => egui::Modal::default_area(id).anchor(egui::Align2::CENTER_CENTER, bounds.center() - screen.center()),
    };
    let mut drag = egui::Vec2::ZERO;
    let mut sizing = false;
    let modal = egui::Modal::new(id).area(area).backdrop_color(egui::Color32::TRANSPARENT).show(ctx, |ui| {
        sizing = ui.is_sizing_pass();
        // Disable the entire older modal, not just its body.
        if opts.inactive {
            ui.disable();
        }
        let margin = ui.spacing().menu_margin;
        let chrome = margin.sum() + egui::vec2(2.0, 2.0);
        let (min_w, max_w) = match size {
            FrameSize::Width(lo, hi) => (lo, hi),
            FrameSize::Fixed(outer) | FrameSize::Panels(outer) => ((outer.x - chrome.x).max(1.0), (outer.x - chrome.x).max(1.0)),
        };
        // Panels sit an even gutter (the frame's margin) from every edge, the title band's too;
        // the other bodies get more room above and below.
        let panels = matches!(size, FrameSize::Panels(_));
        let (gap, pad) = if panels { (0.0, 0.0) } else { (10.0, f32::from(BOTTOM_PAD)) };
        ui.set_min_width(min_w);
        ui.set_max_width(max_w);
        // Title bar: a strip across the dialog's top in the window title bar's colour, its
        // bottom edge the separator. Painted behind the title, so its slot is reserved first.
        let tk = crate::theme::Tokens::get(ui.ctx());
        let strip = ui.painter().clone().with_clip_rect(ui.ctx().content_rect());
        let strip_bg = strip.add(egui::Shape::Noop);
        ui.add_space(1.0);
        let t = ui.add(egui::Label::new(egui::RichText::new(title).font(crate::theme::semibold(13.0)).color(tk.text)).selectable(false)).rect;
        let frame_top = ui.max_rect().top() - f32::from(margin.top);
        let band_bottom = t.bottom() + 7.0;
        ui.add_space(band_bottom - t.bottom() + gap);
        let body_size = size.outer().map(|outer| {
            let used = ui.min_rect().height() + ui.spacing().item_spacing.y;
            egui::vec2(max_w, (outer.y - chrome.y - used - pad).max(1.0))
        });
        let out = body(ui, body_size);
        if pad > 0.0 {
            ui.add_space(pad);
        }
        // Sized after the body: content wider than the frame's maximum widens the dialog, and
        // the band must span what is actually shown.
        let right = ui.max_rect().right().max(ui.min_rect().right());
        let band = egui::Rect::from_min_max(
            egui::pos2(ui.max_rect().left() - f32::from(margin.left), frame_top),
            egui::pos2(right + f32::from(margin.right), band_bottom),
        );
        let r = ui.visuals().menu_corner_radius;
        strip.set(strip_bg, egui::Shape::rect_filled(band, egui::CornerRadius { nw: r.nw, ne: r.ne, sw: 0, se: 0 }, tk.chrome));
        strip.line_segment([band.left_bottom(), band.right_bottom()], egui::Stroke::new(1.0, tk.separator));
        drag = ui.interact(band, id.with("title"), egui::Sense::drag()).drag_delta();
        out
    });
    // Pin once laid out at its real size (the first frame is an invisible sizing pass). A drag
    // moves it from where it is shown and stops at the bounds; a window that shrank brings it
    // back inside.
    if !sizing {
        let moved = modal.response.rect.translate(drag);
        let inside = crate::widgets::keep_inside(moved, bounds);
        if pinned.is_none() || drag != egui::Vec2::ZERO || inside != egui::Vec2::ZERO {
            let offset = moved.translate(inside).min - screen.min;
            if offset.is_finite() {
                ctx.data_mut(|m| m.insert_temp(id, offset));
                if drag != egui::Vec2::ZERO
                    && let Some(place) = opts.place
                {
                    ctx.data_mut(|m| m.insert_persisted(place, offset));
                }
            }
        }
    }
    modal
}

/// A click or drag with the primary button started on the free canvas under an open dialog, Space
/// not held: where the pointer is this frame. The press itself counts even when it is released in
/// the same frame (a quick click, `ui.click`); the drag only while it stays on the free canvas.
pub fn free_press(ctx: &egui::Context, canvas: egui::Rect) -> Option<egui::Pos2> {
    if egui::Popup::is_any_open(ctx) {
        return None;
    }
    let rects = rects(ctx);
    let free = |p: egui::Pos2| canvas.contains(p) && !rects.iter().any(|r| r.contains(p));
    ctx.input(|i| {
        if i.key_down(egui::Key::Space) {
            return None;
        }
        let pressed = i.events.iter().rev().find_map(|e| match e {
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, .. } => Some(*pos),
            _ => None,
        });
        if let Some(p) = pressed {
            return free(p).then_some(p);
        }
        let held = i.pointer.primary_down() && i.pointer.press_origin().is_some_and(free);
        i.pointer.latest_pos().filter(|p| held && free(*p))
    })
}

/// Height of a dialog's title, rule, button row and frame margins (points), around a body that
/// scrolls.
pub(crate) const DIALOG_CHROME: f32 = 150.0;

/// Draw a dialog body that opens on its first number, selected, as Photoshop's value dialogs do:
/// typing replaces it and Enter applies it (#1757). Once per dialog `id`, on the first laid-out
/// frame.
fn first_field_focused<R>(ui: &mut egui::Ui, id: egui::Id, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let ctx = ui.ctx().clone();
    let focused = id.with("first-field");
    let first = !ui.is_sizing_pass() && !ctx.data(|m| m.get_temp::<bool>(focused).unwrap_or(false));
    if first {
        ctx.data_mut(|m| m.insert_temp(focused, true));
    }
    crate::widgets::focus_first_field(&ctx, first);
    let r = body(ui);
    crate::widgets::focus_first_field(&ctx, false);
    r
}

/// Where a dialog's last position is remembered: per command for command dialogs (each filter has
/// its own), per kind for the rest.
fn place_id(d: &Dialog) -> egui::Id {
    match d.fields.get("__command").and_then(Value::as_str) {
        Some(cmd) => egui::Id::new(("dialog-place", cmd)),
        None => egui::Id::new(("dialog-place", format!("{:?}", d.kind))),
    }
}

/// Whether another dialog is drawn above `live` (egui's layer order, back to front).
fn older_dialog_above(ctx: &egui::Context, live: egui::LayerId, dialogs: &[Dialog]) -> bool {
    let others: Vec<egui::LayerId> =
        dialogs.iter().map(|d| egui::LayerId::new(egui::Order::Foreground, egui::Id::new(("dialog", d.id)))).filter(|l| *l != live).collect();
    ctx.memory(|m| m.layer_ids().skip_while(|l| *l != live).skip(1).any(|l| others.contains(&l)))
}

/// Whether floating windows have a close box (×) in their title row (#5496), as Photoshop's do on
/// Windows (none on macOS; Linux and the web follow Windows). The dialog frame's title strip
/// keeps PhotoSplice's look without one: Esc and Cancel close it.
pub(crate) const CLOSE_BOX: bool = !cfg!(target_os = "macos");

/// The dialogs Reference opens as windows of their own (as PhotoSplice does for its large tool
/// dialogs, [`frame`]): Layer Style, New Document and Preferences.
fn floats(d: &Dialog) -> bool {
    matches!(d.kind, DialogKind::LayerStyle | DialogKind::NewDocument) || (d.kind == DialogKind::Command && crate::prefs_ui::is_preferences(&d.fields))
}

pub fn show(app: &mut PhotospliceApp, ctx: &egui::Context) {
    note_host_rect(ctx);
    crate::layer_style::color_picker::prune(app);
    crate::type_panels_ui::color_picker::prune(app);
    crate::edit_history::track_dialogs(app, ctx);
    let dialogs = app.ui.dialogs.clone();
    let top = dialogs.last().map(|d| d.id);
    // From the first one that floats, every dialog does: one opened over it (the Color Picker
    // over Layer Style) would otherwise sit in the main window, behind it.
    let float_from = if ctx.embed_viewports() { None } else { dialogs.iter().position(floats) };
    let front = egui::ViewportId::from_hash_of(egui::Id::new(("dialog", top.unwrap_or_default())));
    let mut shown = Vec::new();
    for (index, d) in dialogs.into_iter().enumerate() {
        let floating = float_from.is_some_and(|f| index >= f);
        // Preferences previews the language picked in it, without changing other dialogs.
        let lang = if crate::prefs_ui::is_preferences(&d.fields) {
            crate::i18n::Lang::from_pref(d.fields.get("values").and_then(|v| v.pointer("/interface/language")).and_then(Value::as_str).unwrap_or("auto"))
        } else {
            crate::i18n::current()
        };
        let _language = crate::i18n::language_scope(lang);
        let interactive = top == Some(d.id);
        let mut fields = d.fields.clone();
        let mut outcome: Option<bool> = None; // Some(true)=OK, Some(false)=Cancel
        let mut apply_requested = false;
        let title = display_title(&d);
        let id = egui::Id::new(("dialog", d.id));
        // A dialog of a few short fields (`__compact`, Fit Image) is narrower (#4673); the curve
        // editor caps its graph allocation at 300 points (#5113).
        let compact = d.fields.contains_key("__compact");
        let curves = fields.get("__adjust").and_then(Value::as_str) == Some("curves");
        let wide = crate::prefs_ui::width(&d.fields, (ctx.content_rect().width() - 48.0).max(380.0));
        let mut min_w = wide.map_or(
            if compact {
                0.0
            } else if curves {
                300.0
            } else {
                380.0
            },
            |w| w.clamp(380.0, 460.0),
        );
        if d.kind == DialogKind::NewDocument {
            min_w = min_w.max(800.0);
        }
        let mut max_w = wide.unwrap_or(if d.kind == DialogKind::NewDocument {
            800.0
        } else if d.kind == DialogKind::LayerStyle {
            // Effect list, parameters, and the Preview swatch column.
            712.0
        } else if crate::color_picker_ui::owns(&d.fields) {
            min_w = crate::color_picker_ui::WIDTH;
            crate::color_picker_ui::WIDTH
        } else if d.kind == DialogKind::About && crate::notices_ui::owns(&d.fields) {
            min_w = crate::notices_ui::WIDTH;
            crate::notices_ui::WIDTH
        } else if d.fields.contains_key("__export") {
            600.0
        } else if compact {
            260.0
        } else if curves {
            300.0
        } else {
            440.0
        });
        if let Some(w) = crate::file_ui::dialog_width(&d.fields) {
            min_w = min_w.max(w);
            max_w = w;
        }
        // Duplicate Layer… is a fixed small grid (labels + 260 pt fields ≈ 352 pt): the generic
        // 380 pt minimum left a dead strip on its right.
        if crate::duplicate_layer_ui::owns(&d.fields) {
            (min_w, max_w) = (360.0, 360.0);
        }
        // Only the last dialog takes input, and egui only lets the modal drawn on top take input,
        // so the last one must be drawn above the others. egui can raise an older one (it raises an
        // area that is pressed or wasn't drawn the frame before), which left a greyed-out dialog
        // in front and the live one unreachable behind it. Raise the live one only then: every
        // frame would also cover its own popups.
        let layer = egui::LayerId::new(egui::Order::Foreground, id);
        if !floating && interactive && older_dialog_above(ctx, layer, &app.ui.dialogs) {
            ctx.move_to_top(layer);
        }
        let web = fields.contains_key("__web");
        let draw = |ui: &mut egui::Ui, _: Option<egui::Vec2>| {
            // Read where the dialog is drawn (its own window, when it floats).
            // Tab / ⇧Tab walk the topmost dialog's text fields, not every widget (field_tab.rs).
            let tab = if interactive && !egui::Popup::is_any_open(ui.ctx()) { crate::field_tab::take_step(ui.ctx()) } else { 0 };
            let web_popup_open = web && egui::Popup::is_any_open(ui.ctx());
            crate::field_tab::begin(ui.ctx());
            match d.kind {
                DialogKind::NewDocument => {
                    if crate::new_doc_ui::body(app, ui, &mut fields) {
                        outcome = Some(true);
                    }
                }
                DialogKind::About if fields.get("systemInfo").and_then(Value::as_bool) == Some(true) => {
                    let lines = crate::gpu_status::system_info_ui(app);
                    for l in &lines {
                        ui.add(egui::Label::new(egui::RichText::new(l).font(crate::theme::mono(12.0))).selectable(true));
                    }
                    ui.add_space(8.0);
                    if crate::widgets::secondary_button(ui, tl!("Copy"), 84.0).clicked() {
                        ui.ctx().copy_text(crate::gpu_status::system_info(app).join("\n"));
                    }
                }
                DialogKind::About if crate::notices_ui::owns(&fields) => {
                    if let Some(url) = crate::notices_ui::body(ui, &mut fields) {
                        let ctx = ui.ctx().clone();
                        crate::links::open(app, &ctx, &url);
                    }
                }
                DialogKind::About => {
                    ui.label(tl!("PhotoSplice — an open-source, native image editor written in Rust."));
                    ui.label(crate::i18n::fmt(tl!("Version {version}"), &[("version", &photosplice_engine::build_info::long_version())]));
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| {
                        crate::links::link_row(app, ui);
                    });
                    ui.add_space(10.0);
                    ui.weak("egui · wgpu · photosplice-engine");
                }
                DialogKind::Command if crate::guide_layout_ui::owns(&fields) => outcome = crate::guide_layout_ui::body(app, ui, &mut fields, interactive),
                DialogKind::Command if crate::brush_preset_dialog::owns(&fields) => {
                    outcome = crate::brush_preset_dialog::body(app, ui, &mut fields, interactive)
                }
                DialogKind::Command if crate::fill_ui::owns(&fields) => crate::fill_ui::body(app, ui, &mut fields),
                DialogKind::Command if crate::pattern_fill_ui::owns(&fields) => crate::pattern_fill_ui::body(app, ui, &mut fields),
                DialogKind::Command if crate::stroke_ui::owns(&fields) => crate::stroke_ui::body(ui, &mut fields),
                DialogKind::Command if crate::channel_options_ui::owns(&fields) => crate::channel_options_ui::body(ui, &mut fields),
                DialogKind::Command if crate::channel_options_ui::owns_new_spot(&fields) => crate::channel_options_ui::new_spot_body(ui, &mut fields),
                DialogKind::Command if crate::shape_dialog::owns(&fields) => crate::shape_dialog::body(app, ui, &mut fields),
                DialogKind::Command if crate::delete_layer_prompt::owns(&fields) => crate::delete_layer_prompt::body(ui, &mut fields),
                DialogKind::Command if crate::actions::owns_insert_stop(&fields) => crate::actions::insert_stop_body(ui, &mut fields),
                DialogKind::Command if crate::new_layer_ui::owns(&fields) => crate::new_layer_ui::body(ui, &mut fields),
                DialogKind::Command if crate::mask_trash_prompt::owns(&fields) => crate::mask_trash_prompt::body(ui, &fields),
                DialogKind::Command if crate::rasterize_prompt::owns(&fields) => crate::rasterize_prompt::body(ui, &fields),
                DialogKind::Command if crate::duplicate_layer_ui::owns(&fields) => crate::duplicate_layer_ui::body(app, ui, &mut fields),
                DialogKind::Command if crate::stack_dialog::owns(&fields) => crate::stack_dialog::body(app, ui, &mut fields, d.id),
                DialogKind::Command if crate::tool_reset::owns(&fields) => crate::tool_reset::body(ui),
                DialogKind::Command if crate::crop_presets::owns(&fields) => crate::crop_presets::body(app, ui, &mut fields),
                DialogKind::Command if crate::variables_ui::owns(&fields) => crate::variables_ui::body(app, ui, &mut fields),
                DialogKind::Command if crate::file_ui::owns(&fields) => crate::file_ui::body(app, ui, &mut fields),
                // The Color Picker draws its own OK / Cancel, top right as in Photoshop.
                DialogKind::Command if crate::color_picker_ui::owns(&fields) => {
                    outcome = crate::color_picker_ui::body(ui, &mut fields);
                    crate::color_picker_ui::take_add_swatch(app, &mut fields);
                }
                DialogKind::Command if crate::color_range_ui::owns(&fields) => crate::color_range_ui::body(app, ui, &mut fields),
                DialogKind::Command if crate::prefs_ui::owns(&fields) => crate::prefs_ui::body(app, ui, &mut fields),
                DialogKind::Command if fields.contains_key("__export") => crate::export_dialog::body(app, ui, &mut fields),
                // Image Size and Canvas Size open on Width, selected (#2329).
                DialogKind::Command if fields.contains_key("__sizing") => first_field_focused(ui, id, |ui| crate::sizing::body(ui, &mut fields)),
                DialogKind::Command if crate::adjust_ui::owns_lookup(&fields) => crate::adjust_ui::lookup_dialog_body(app, ui, &mut fields),
                DialogKind::Command if crate::adjust_dialog::owns(&fields) => crate::adjust_dialog::body(app, ui, &mut fields),
                DialogKind::Command if crate::blur_pins::owns(&fields) => crate::blur_pins::body(ui, &mut fields),
                DialogKind::Command if fields.contains_key("__filter") => {
                    // Color Settings: the monitor profile in use can change while it is open.
                    if fields.get("__command").and_then(Value::as_str) == Some("edit.colorSettings") {
                        fields.insert("__note".into(), Value::String(crate::monitor_status::note(app)));
                    }
                    // Long parameter lists (Flame, Lighting Effects) scroll, so the title and the
                    // OK / Cancel buttons stay inside a small window.
                    // The preview window below the parameters reserves its own space (#3769).
                    let room = (ctx.content_rect().height() - DIALOG_CHROME - crate::filter_preview_ui::reserved_height(ctx, &fields)).max(120.0);
                    let body = |ui: &mut egui::Ui, fields: &mut serde_json::Map<String, Value>| {
                        egui::ScrollArea::vertical().id_salt(id.with("body")).max_height(room).show(ui, |ui| crate::filter_dialog::body(ui, fields));
                        // The in-dialog preview window (full size) sits under the scrolling
                        // parameters, so it never shifts them.
                        crate::filter_preview_ui::window(app, d.id, ui, fields);
                    };
                    // Select and Mask leaves the keyboard to its view keys (F, X, O, V…).
                    if crate::select_mask_view::owns(&fields) {
                        body(ui, &mut fields);
                    } else {
                        first_field_focused(ui, id, |ui| body(ui, &mut fields));
                    }
                }
                DialogKind::Command if crate::new_guide_ui::owns(&fields) => crate::new_guide_ui::body(ui, &mut fields),
                DialogKind::Command if fields.contains_key("__form") => crate::view_cmds::form_body(ui, &mut fields),
                DialogKind::Command => {}
                DialogKind::LayerStyle => {
                    let styles = crate::layer_style::style_choices(app, ui.ctx());
                    crate::layer_style::body(ui, &mut fields, &app.session.patterns, &styles)
                }
                DialogKind::Error => {
                    ui.label(fields.get("message").and_then(Value::as_str).unwrap_or("Error"));
                }
            }
            crate::field_tab::end(ui.ctx(), tab);
            if crate::guide_layout_ui::owns(&fields) || crate::brush_preset_dialog::owns(&fields) {
                // These dialogs own their right-side actions and validate Enter themselves.
            } else if crate::color_picker_ui::owns(&fields) {
                if interactive && outcome.is_none() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    outcome = Some(true);
                }
            } else {
                ui.add_space(8.0);
                // Align::Min, not Center: a centred row fills the height left over from last frame's
                // (larger) size, so a dialog whose body gets shorter would never shrink back.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if matches!(d.kind, DialogKind::About | DialogKind::Error) {
                        // ↩ is OK too, as on every other dialog (#4872), from the frame after the
                        // alert appeared: the ↩ that raised it (committing a locked layer's Free
                        // Transform, say) must not close it again at once.
                        let seen = id.with("alert-seen");
                        let shown_before = ui.ctx().data(|m| m.get_temp::<bool>(seen).unwrap_or(false));
                        ui.ctx().data_mut(|m| m.insert_temp(seen, true));
                        let enter = interactive && shown_before && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if dialog_buttons(ui, &[DialogButton::new(ButtonRole::Default, tl!("OK"), 84.0)]).is_some() || enter {
                            outcome = Some(false);
                        }
                    } else {
                        let ok_label = if d.kind == DialogKind::NewDocument {
                            tl!("Create")
                        } else if crate::delete_layer_prompt::owns(&d.fields) {
                            tl!("Delete")
                        } else if d.fields.contains_key("__export") {
                            tl!("Export")
                        } else {
                            crate::file_ui::ok_label(&d.fields).unwrap_or(tl!("OK"))
                        };
                        // Dialogs that need an input first (a preset name, a selection) grey OK out.
                        let can_ok = crate::crop_presets::can_confirm(app, &fields) && crate::stack_dialog::can_confirm(&fields);
                        let ok = DialogButton::new(ButtonRole::Default, ok_label, 84.0).enabled(can_ok);
                        // Photoshop: holding Alt turns Cancel into Reset (the dialog stays open).
                        // Dialogs opt in via `dialog_alt_reset::supported` (per marker).
                        let reset = ui.input(|i| i.modifiers.alt) && crate::dialog_alt_reset::supported(&fields);
                        let cancel_label = if reset {
                            tl!("Reset")
                        } else if d.kind == DialogKind::NewDocument {
                            tl!("Close")
                        } else {
                            tl!("Cancel")
                        };
                        let cancel = DialogButton::new(ButtonRole::Cancel, cancel_label, 84.0);
                        let clicked = if d.kind == DialogKind::Command && crate::prefs_ui::is_preferences(&fields) {
                            let changed = crate::prefs_ui::preferences_changed(app, &fields);
                            dialog_buttons(ui, &[ok, cancel, DialogButton::new(ButtonRole::Apply, tl!("Apply"), 84.0).enabled(changed)])
                        } else if crate::mask_trash_prompt::owns(&fields) && !crate::mask_trash_prompt::is_vector(&fields) {
                            // Photoshop: Apply / Cancel / Delete. Delete answers with the choice set.
                            let apply = DialogButton::new(ButtonRole::Default, tl!("Apply"), 84.0).enabled(crate::mask_trash_prompt::can_apply(&fields));
                            let delete = DialogButton::new(ButtonRole::Alternate, tl!("Delete"), 84.0);
                            let hit = dialog_buttons(ui, &[apply, cancel, delete]);
                            if hit == Some(ButtonRole::Alternate) {
                                fields.insert("choice".into(), json!("delete"));
                            } else if hit == Some(ButtonRole::Default) {
                                fields.insert("choice".into(), json!("apply"));
                            }
                            hit
                        } else if crate::rasterize_prompt::is_filter(&fields) {
                            // Photoshop: Convert to Smart Object / Rasterize / Cancel (#4303).
                            let rasterize = DialogButton::new(ButtonRole::Default, tl!("Rasterize"), 84.0);
                            let hit = if crate::rasterize_prompt::offers_smart_object(&fields) {
                                let convert = DialogButton::new(ButtonRole::Alternate, tl!("Convert to Smart Object"), 160.0);
                                dialog_buttons(ui, &[rasterize, cancel, convert])
                            } else {
                                dialog_buttons(ui, &[rasterize, cancel])
                            };
                            if hit == Some(ButtonRole::Alternate) {
                                fields.insert("choice".into(), json!("smartObject"));
                            } else if hit == Some(ButtonRole::Default) {
                                fields.insert("choice".into(), json!("rasterize"));
                            }
                            hit
                        } else {
                            dialog_buttons(ui, &[ok, cancel])
                        };
                        match clicked {
                            Some(ButtonRole::Cancel) if reset => crate::dialog_alt_reset::apply(app, ui.ctx(), &mut fields),
                            Some(ButtonRole::Cancel) => outcome = Some(false),
                            Some(ButtonRole::Apply) => apply_requested = true,
                            Some(_) => outcome = Some(true),
                            None if interactive
                                && can_ok
                                && (!web || (!web_popup_open && !egui::Popup::is_any_open(ui.ctx())))
                                && ui.input(|i| i.key_pressed(egui::Key::Enter)) =>
                            {
                                outcome = Some(true)
                            }
                            None => {}
                        }
                    }
                });
            }
            // The frame around the content (Frame::popup's margin and stroke).
            let r = ui.min_rect().expand(ui.spacing().menu_margin.sum().max_elem() + 2.0);
            egui::Rect::from_min_max(r.min, r.max + egui::vec2(0.0, f32::from(BOTTOM_PAD)))
        };
        // Esc cancels (topmost dialog, no popup open). A click outside does nothing: Reference keeps
        // the dialog, and the pointer may be panning or zooming the canvas under it.
        let close = if crate::lens_blur_ui::owns(&d.fields) {
            // Lens Blur: a large dialog in a window of its own, with its own OK / Cancel.
            outcome = crate::lens_blur_ui::show(app, ctx, d.id, &title, &mut fields, interactive);
            false
        } else if floating {
            let opts = FloatOpts { place: place_id(&d), inactive: !interactive, front, forward_keys: false, bleed: false };
            floating_window(ctx, id, &title, FloatSize::Content(min_w, max_w), opts, draw).close
        } else {
            let opts = FrameOpts { place: Some(place_id(&d)), inactive: !interactive, first: crate::blur_pins::first_place(&d.fields, ctx) };
            let modal = frame_with(ctx, id, &title, FrameSize::Width(min_w, max_w), opts, draw);
            shown.push(modal.inner);
            modal.response.should_close()
                || (modal.is_top_modal && !modal.any_popup_open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)))
        };
        if interactive && outcome.is_none() && close {
            outcome = Some(false);
        }
        if let Some(dm) = app.ui.dialog_mut(d.id) {
            dm.fields = fields;
        }
        // A colour swatch (Edit › Fill, Edit › Stroke) opens the Color Picker over the dialog.
        if outcome.is_none() {
            crate::color_picker_ui::open_requested(app, d.id);
        }
        if apply_requested && outcome.is_none() {
            let _ = crate::prefs_ui::apply(app, d.id);
        }
        match outcome {
            Some(true) => {
                if let Err(error) = confirm(app, d.id) {
                    app.ui.status = error;
                }
            }
            Some(false) => {
                let _ = cancel(app, d.id);
            }
            None => {
                if let Err(error) = crate::layer_style::color_picker::take_request(app, d.id) {
                    app.ui.status = error;
                }
            }
        }
    }
    ctx.data_mut(|m| m.insert_temp(egui::Id::new(RECTS), shown));
}

/// The dialog title as shown: [`title`] in the UI language.
fn display_title(d: &Dialog) -> String {
    match d.kind {
        DialogKind::Command => {
            let label = d.fields.get("__label").and_then(Value::as_str).unwrap_or("Command");
            // Dialog labels are catalogued with their "…" ("Export As…"); some commands omit it.
            let with_dots = format!("{}…", label.trim_end_matches('…'));
            let shown = if crate::i18n::has(crate::i18n::current(), label) { tl!(label) } else { tl!(&with_dots) };
            shown.trim_end_matches('…').to_string()
        }
        _ => tl!(&title(d)).to_string(),
    }
}

pub fn title(d: &Dialog) -> String {
    match d.kind {
        DialogKind::NewDocument => "New Document".into(),
        DialogKind::About if d.fields.get("systemInfo").and_then(Value::as_bool) == Some(true) => "System Info".into(),
        DialogKind::About if crate::notices_ui::owns(&d.fields) => "Third-Party Notices".into(),
        DialogKind::About => "About PhotoSplice".into(),
        DialogKind::LayerStyle => "Layer Style".into(),
        DialogKind::Command => d.fields.get("__label").and_then(Value::as_str).unwrap_or("Command").trim_end_matches('…').to_string(),
        DialogKind::Error => "Error".into(),
    }
}

/// Confirm a dialog: run its action and close it. Used by the OK button and by automation.
pub fn confirm(app: &mut PhotospliceApp, id: u64) -> Result<Value, String> {
    if crate::layer_style::color_picker::has_child(app, id) {
        return Err("confirm or cancel the Layer Style color picker first".into());
    }
    if let Some(d) = app.ui.dialogs.iter().find(|d| d.id == id).filter(|d| crate::guide_layout_ui::owns(&d.fields)) {
        if app.ui.dialogs.iter().any(|child| crate::color_picker_ui::owns(&child.fields) && child.fields.get("__dialog").and_then(Value::as_u64) == Some(id)) {
            return Err("confirm or cancel the guide color picker first".into());
        }
        crate::guide_layout_ui::validate(app, &d.fields)?;
    }
    let d = app.ui.close_dialog(id).ok_or_else(|| format!("no dialog {id}"))?;
    match d.kind {
        DialogKind::NewDocument => {
            let r = app.run("file.new", crate::new_doc_ui::command_params(&d.fields));
            if r.is_ok() {
                crate::new_doc_ui::remember(app, &d.fields);
            }
            if let Some(i) = app.session.active_index() {
                app.ui.views[i].fit_pending = true;
            }
            r
        }
        DialogKind::Command if crate::guide_layout_ui::owns(&d.fields) => crate::guide_layout_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::brush_preset_dialog::owns(&d.fields) => crate::brush_preset_dialog::confirm(app, &d.fields),
        DialogKind::Command if crate::fill_ui::owns(&d.fields) => crate::fill_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::pattern_fill_ui::owns(&d.fields) => crate::pattern_fill_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::stroke_ui::owns(&d.fields) => crate::stroke_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::channel_options_ui::owns(&d.fields) => crate::channel_options_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::channel_options_ui::owns_new_spot(&d.fields) => crate::channel_options_ui::confirm_new_spot(app, &d.fields),
        DialogKind::Command if crate::shape_dialog::owns(&d.fields) => crate::shape_dialog::confirm(app, &d.fields),
        DialogKind::Command if crate::delete_layer_prompt::owns(&d.fields) => crate::delete_layer_prompt::confirm(app, &d.fields),
        DialogKind::Command if crate::actions::owns_insert_stop(&d.fields) => crate::actions::insert_stop_confirm(app, &d.fields),
        DialogKind::Command if crate::new_layer_ui::owns(&d.fields) => crate::new_layer_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::mask_trash_prompt::owns(&d.fields) => crate::mask_trash_prompt::confirm(app, &d.fields),
        DialogKind::Command if crate::rasterize_prompt::owns(&d.fields) => crate::rasterize_prompt::confirm(app, &d.fields),
        DialogKind::Command if crate::duplicate_layer_ui::owns(&d.fields) => crate::duplicate_layer_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::stack_dialog::owns(&d.fields) => crate::stack_dialog::confirm(app, &d.fields),
        DialogKind::Command if crate::tool_reset::owns(&d.fields) => crate::tool_reset::confirm(app),
        DialogKind::Command if crate::crop_presets::owns(&d.fields) => crate::crop_presets::confirm(app, &d.fields),
        DialogKind::Command if crate::variables_ui::owns(&d.fields) => crate::variables_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::file_ui::owns(&d.fields) => crate::file_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::new_guide_ui::owns(&d.fields) => crate::new_guide_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::color_picker_ui::owns(&d.fields) => crate::color_picker_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::color_range_ui::owns(&d.fields) => crate::color_range_ui::confirm(app, &d.fields),
        DialogKind::Command if crate::prefs_ui::owns(&d.fields) => crate::prefs_ui::confirm(app, &d.fields),
        DialogKind::Command if d.fields.contains_key("__export") => crate::export_dialog::confirm(app, &d.fields),
        DialogKind::Command => {
            let Some(cmd) = d.fields.get("__command").and_then(Value::as_str).map(str::to_string) else {
                app.filter_preview = None;
                return Err("dialog has no command".into());
            };
            let (cmd, params) = crate::smart_ui::confirm_command(&d.fields, cmd, crate::filter_dialog::params_of(&d.fields));
            let result = app.run(&cmd, params.clone());
            // A filter that runs as a background job keeps its preview on screen until the job
            // lands (`canvas::committed_filter_preview`) instead of flashing the unfiltered image.
            let running = app
                .session
                .active()
                .is_some_and(|st| app.filter_preview.as_ref().is_some_and(|p| p.key.doc == st.doc.id) && app.session.job_on(st.doc.id).is_some());
            match app.filter_preview.as_mut() {
                Some(p) if result.is_ok() && running => p.committing = true,
                _ => app.filter_preview = None,
            }
            if result.is_ok() && cmd == "view.newGuideLayout" {
                app.ui.view.guide_layout = params;
            }
            if result.is_ok() {
                crate::filter_dialog::remember(app, &cmd, &d.fields);
                // A step dialog of a playing action: the action continues after it.
                crate::actions::resume(app, &d.fields);
            }
            result
        }
        DialogKind::LayerStyle => crate::layer_style::confirm(app, &d.fields),
        DialogKind::About | DialogKind::Error => Ok(Value::Null),
    }
}

/// Cancel a dialog and its dependent Layer Style picker, without applying edits.
pub fn cancel(app: &mut PhotospliceApp, id: u64) -> Result<Value, String> {
    let closed = app.ui.close_dialog(id).ok_or_else(|| format!("no dialog {id}"))?;
    crate::actions::stopped(app, &closed.fields);
    if crate::guide_layout_ui::owns(&closed.fields) {
        app.ui.dialogs.retain(|child| child.fields.get("__dialog").and_then(Value::as_u64) != Some(id));
    }
    // Cancel on a profile prompt opens neither its file nor the ones still to come.
    if crate::open_prompt::owns(&closed.fields) {
        crate::open_prompt::answer(app, None)?;
    }
    app.ui.dialogs.retain(|d| !crate::layer_style::color_picker::child_of(&d.fields, id));
    app.filter_preview = None;
    app.color_range = None;
    crate::type_panels_ui::color_picker::prune(app);
    Ok(Value::Null)
}

/// Open the parameter dialog of `command`: the adjustment editor for `image.adjustments.*`, the
/// schema dialog for filters, the Color Range dialog for `select.colorRange`, otherwise a bare
/// confirm dialog.
pub fn open_command_dialog(app: &mut PhotospliceApp, command: &str, label: &str) -> u64 {
    if command == "view.newGuideLayout" {
        return crate::guide_layout_ui::open(app);
    }
    if command == crate::stroke_ui::COMMAND {
        return crate::stroke_ui::open(app);
    }
    if command == crate::color_range_ui::COMMAND {
        return crate::color_range_ui::open(app);
    }
    if let Some(id) = crate::adjust_dialog::open(app, command) {
        return id;
    }
    if crate::filter_dialog::has_dialog(command)
        && let Some(id) = crate::filter_dialog::open(app, command)
    {
        return id;
    }
    let mut fields = serde_json::Map::new();
    fields.insert("__command".into(), json!(command));
    fields.insert("__label".into(), json!(label));
    app.ui.open_dialog(DialogKind::Command, fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A floating dialog's key presses reach the main window's input on the next frame, once, and
    /// never another window's.
    #[test]
    fn floating_dialog_keys_go_to_the_main_window_once() {
        let ctx = egui::Context::default();
        let enter = egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        ctx.data_mut(|m| m.insert_temp(egui::Id::new(FLOATING_KEYS), FloatingKeys { events: vec![enter.clone()] }));
        let mut other = egui::RawInput { viewport_id: egui::ViewportId::from_hash_of("dialog"), ..Default::default() };
        forward_floating_keys(&ctx, &mut other);
        assert!(other.events.is_empty(), "only the main window takes them");
        let mut root = egui::RawInput::default();
        forward_floating_keys(&ctx, &mut root);
        assert_eq!(root.events, vec![enter]);
        let mut again = egui::RawInput::default();
        forward_floating_keys(&ctx, &mut again);
        assert!(again.events.is_empty(), "handed over once");
    }

    /// Layer Style, New Document and Preferences open as windows of their own, as in Reference;
    /// the small dialogs (Image Size here) stay in the main window.
    #[test]
    fn the_large_dialogs_float() {
        let dialog = |kind, fields: serde_json::Value| Dialog { id: 1, kind, fields: fields.as_object().cloned().unwrap_or_default() };
        assert!(floats(&dialog(DialogKind::LayerStyle, json!({}))));
        assert!(floats(&dialog(DialogKind::NewDocument, json!({}))));
        assert!(floats(&dialog(DialogKind::Command, json!({"__prefsui": "prefs"}))));
        assert!(!floats(&dialog(DialogKind::Command, json!({"__sizing": true, "__command": "image.imageSize"}))));
        assert!(!floats(&dialog(DialogKind::Error, json!({"message": "x"}))));
    }

    /// In the main window (no separate windows: the web build, tests), a large dialog's frame
    /// asks to close on Esc but not on a click outside it, as Reference keeps the dialog.
    #[test]
    fn a_fixed_frame_closes_on_escape_but_not_on_a_click_outside() {
        use egui_kittest::Harness;
        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        let ready = std::rc::Rc::new(std::cell::Cell::new(false));
        let (seen, drawn) = (closed.clone(), ready.clone());
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 700.0)).build_ui(move |ui| {
            // Drawn once the theme's fonts are in (the harness runs a frame as it is built).
            if !drawn.get() {
                return;
            }
            let r = frame(ui.ctx(), egui::Id::new("big"), "Big", FrameSize::Fixed(egui::vec2(400.0, 300.0)), |ui, size| {
                let _ = ui.allocate_exact_size(size.unwrap_or(egui::vec2(10.0, 10.0)), egui::Sense::hover());
            });
            assert!(r.inner.is_some(), "the in-window frame always draws the body");
            seen.set(seen.get() || r.should_close());
        });
        PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::default());
        ready.set(true);
        h.run_steps(2);
        h.drag_at(egui::pos2(10.0, 690.0));
        h.drop_at(egui::pos2(10.0, 690.0));
        h.run_steps(2);
        assert!(!closed.get(), "a click outside keeps it");
        h.key_press(egui::Key::Escape);
        h.run_steps(1);
        assert!(closed.get(), "Esc closes it");
    }

    /// A dialog opens centred below the title bar (its first frame, when its size isn't known
    /// yet, used to leave it pinned to a corner), and Camera Raw gets the same frame: the same
    /// title bar as every other dialog.
    #[test]
    fn dialogs_open_centred_and_share_one_frame() {
        use egui_kittest::{Harness, kittest::Queryable};

        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        app.run("file.new", serde_json::json!({"width": 64, "height": 48})).unwrap();
        let mut h = Harness::builder().with_size(egui::vec2(1440.0, 900.0)).build_ui_state(
            |ui, app| {
                show(app, ui.ctx());
                crate::camera_raw_ui::show(app, ui.ctx());
            },
            app,
        );
        PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        h.state_mut().ui.open_dialog(DialogKind::LayerStyle, serde_json::Map::new());
        h.run_steps(4);
        let bounds = crate::widgets::dialog_bounds(&h.ctx);
        let title = h.get_by_label("Layer Style").rect();
        // The button row's right edge: OK or Cancel, whichever the platform's order puts last.
        let ok = h.get_by_label("OK").rect().union(h.get_by_label("Cancel").rect());
        // Centred: the title's left edge and the button row's right edge are about as far from the
        // window's sides.
        let (left, right) = (title.left() - bounds.left(), bounds.right() - ok.right());
        assert!((left - right).abs() < 24.0 && left > 100.0, "left {left}, right {right}");
        let layer_style_title = title.height();
        h.state_mut().ui.dialogs.clear();
        let ctx = h.ctx.clone();
        crate::camera_raw_ui::menu(h.state_mut(), &ctx, "filter.cameraRaw", &serde_json::json!({})).unwrap().unwrap();
        h.run_steps(4);
        let cr = h.get_by_label_contains("Camera Raw Filter").rect();
        assert!((cr.height() - layer_style_title).abs() < 0.5, "same title font: {} vs {layer_style_title}", cr.height());
        let cr_ok = h.get_by_label("OK").rect().union(h.get_by_label("Cancel").rect());
        let (left, right) = (cr.left() - bounds.left(), bounds.right() - cr_ok.right());
        assert!((left - right).abs() < 24.0 && left > 16.0, "Camera Raw: left {left}, right {right}");
    }

    /// A filter dialog over a 256×256 layer, with the preview the canvas computed for it on screen.
    fn filter_dialog_with_preview(background: bool) -> (PhotospliceApp, u64) {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        app.background_jobs = background;
        app.run("file.new", serde_json::json!({"width": 256, "height": 256, "background": "#3366cc"})).unwrap();
        let id = open_command_dialog(&mut app, "filter.blur.gaussianBlur", "Gaussian Blur");
        let st = app.session.active().unwrap();
        let result = crate::filter_dialog::preview_document(&st.doc, st.active_layer, "filter.blur.gaussianBlur", &serde_json::json!({"radius": 4.0}), 1);
        let key = crate::filter_dialog::FilterPreviewKey {
            doc: st.doc.id,
            revision: st.revision,
            dialog: id,
            active: st.active_layer,
            target: st.channel_view.target,
            command: "filter.blur.gaussianBlur".into(),
            params: serde_json::json!({"radius": 4.0}),
            k: 1,
        };
        app.filter_preview = Some(crate::filter_dialog::FilterPreview { key, result: result.map(std::sync::Arc::new), committing: false });
        (app, id)
    }

    #[test]
    fn a_filters_preview_stays_on_screen_until_its_job_lands() {
        // OK dropped the preview and the filter ran as a background job, so the canvas flashed
        // the unfiltered image until the job finished.
        let (mut app, id) = filter_dialog_with_preview(true);
        let revision = app.session.active().unwrap().revision;
        confirm(&mut app, id).unwrap();
        let doc = app.session.active().unwrap().doc.id;
        let job = app.session.job_on(doc).expect("the blur runs as a background job").id;
        assert!(matches!(crate::canvas::committed_filter_preview(&mut app, 0), Some(Some((1, _, [256, 256])))), "the preview is shown");
        // The job lands: the document changes and the preview goes.
        app.session.wait_job(job).unwrap();
        assert_ne!(app.session.active().unwrap().revision, revision);
        assert_eq!(crate::canvas::committed_filter_preview(&mut app, 0), Some(None));
        assert!(app.filter_preview.is_none());

        // A cancelled job leaves the document as it was, so its preview goes too.
        let (mut app, id) = filter_dialog_with_preview(true);
        confirm(&mut app, id).unwrap();
        let doc = app.session.active().unwrap().doc.id;
        let job = app.session.job_on(doc).unwrap().id;
        assert!(matches!(crate::canvas::committed_filter_preview(&mut app, 0), Some(Some(_))));
        assert!(app.session.cancel_job(job));
        assert_eq!(crate::canvas::committed_filter_preview(&mut app, 0), Some(None));
        assert!(app.filter_preview.is_none());

        // Inline (no background jobs): the filter has already landed, nothing is held.
        let (mut app, id) = filter_dialog_with_preview(false);
        confirm(&mut app, id).unwrap();
        assert!(app.filter_preview.is_none());
        assert_eq!(crate::canvas::committed_filter_preview(&mut app, 0), None);
    }

    #[test]
    fn layer_style_title_gutters_can_start_drags() {
        use egui_kittest::{Harness, kittest::Queryable};

        for point in 0..5 {
            let app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
            let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_ui_state(|ui, app| show(app, ui.ctx()), app);
            PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
            h.state_mut().ui.open_dialog(DialogKind::LayerStyle, serde_json::Map::new());
            h.run_steps(3);
            let before = h.get_by_label("Layer Style").rect();
            let from = match point {
                0 => egui::pos2(before.left() - 5.0, before.bottom() - 4.0),
                1 => egui::pos2(before.right() + 5.0, before.bottom() - 4.0),
                2 => egui::pos2(before.center().x, before.bottom() + 3.0),
                3 => egui::pos2(before.center().x, before.top() - 5.0),
                _ => egui::pos2(before.center().x, before.bottom() - 3.0),
            };
            h.hover_at(from);
            h.drag_at(from);
            h.run_steps(2);
            for i in 1..=8 {
                h.hover_at(from + egui::vec2(64.0, -32.0) * (i as f32 / 8.0));
                h.run_steps(1);
            }
            h.drop_at(from + egui::vec2(64.0, -32.0));
            h.run_steps(3);
            let delta = h.get_by_label("Layer Style").rect().min - before.min;
            assert!((delta - egui::vec2(64.0, -32.0)).length() < 2.0, "point {point}: moved {delta:?}");
            assert_eq!(h.state().ui.dialogs.len(), 1);
        }
    }

    #[test]
    fn dragging_the_title_bar_moves_the_dialog() {
        use egui_kittest::{Harness, kittest::Queryable};

        let app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        let mut harness = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_ui_state(|ui, app| show(app, ui.ctx()), app);
        PhotospliceApp::setup_context(&harness.ctx, crate::theme::ThemeKind::ALL[0]);
        harness.state_mut().ui.open_dialog(DialogKind::LayerStyle, serde_json::Map::new());
        harness.run_steps(3);
        let before = harness.get_by_label("Layer Style").rect();

        // Grab the title text itself: it must move the dialog, not select the text.
        let from = before.center();
        harness.hover_at(from);
        harness.drag_at(from);
        harness.run_steps(2);
        for i in 1..=10 {
            harness.hover_at(from + egui::vec2(-12.0, 8.0) * i as f32);
            harness.run_steps(1);
        }
        harness.drop_at(from + egui::vec2(-120.0, 80.0));
        harness.run_steps(3);

        let moved = harness.get_by_label("Layer Style").rect().min - before.min;
        assert!((moved - egui::vec2(-120.0, 80.0)).length() < 1.0, "dialog moved by {moved:?}");
        assert_eq!(harness.state().ui.dialogs.len(), 1, "dragging must not close the dialog");

        // Dragged far up, it stops below the title bar (on macOS the system's: under the traffic
        // lights, where a drag moves the window).
        let title = harness.get_by_label("Layer Style").rect();
        let from = title.center();
        harness.hover_at(from);
        harness.drag_at(from);
        harness.run_steps(2);
        for i in 1..=10 {
            harness.hover_at(from + egui::vec2(0.0, -150.0) * i as f32);
            harness.run_steps(1);
        }
        harness.drop_at(from + egui::vec2(0.0, -1500.0));
        harness.run_steps(3);
        let top = harness.get_by_label("Layer Style").rect().top();
        assert!(top >= crate::panels::title_bar_height(&harness.ctx), "title at {top}, under the title bar");
    }

    /// #4872: an alert (Error, About) closes with ↩ as with OK, but not on the ↩ that raised it.
    #[test]
    fn enter_closes_an_alert_but_not_the_enter_that_opened_it() {
        let app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(900.0, 600.0)).build_ui_state(|ui, app| show(app, ui.ctx()), app);
        PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        h.run_steps(2);
        let alert = |h: &mut egui_kittest::Harness<'static, PhotospliceApp>| {
            h.state_mut().ui.open_dialog(DialogKind::Error, json!({"message": crate::move_lock::MESSAGE}).as_object().cloned().unwrap_or_default())
        };
        // Raised in the frame its own ↩ arrives: that ↩ leaves it open.
        alert(&mut h);
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        assert_eq!(h.state().ui.dialogs.len(), 1, "the ↩ that raised the alert doesn't close it");
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        assert!(h.state().ui.dialogs.is_empty(), "↩ is OK");
        // A new alert starts over, and Esc still closes it.
        alert(&mut h);
        h.run_steps(2);
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(h.state().ui.dialogs.is_empty(), "Esc too");
    }

    fn dialog_harness(size: egui::Vec2, command: &str) -> egui_kittest::Harness<'static, PhotospliceApp> {
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 64, "height": 64})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(size).build_ui_state(|ui, app| show(app, ui.ctx()), app);
        PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        crate::filter_dialog::open(h.state_mut(), command).unwrap();
        h.run_steps(4);
        h
    }

    /// #3152: Motion Blur's angle is a dial, as in Photoshop. Clicking or dragging it points the
    /// blur; the field still takes the whole -360..360 range.
    #[test]
    fn motion_blur_angle_dial_sets_the_angle_by_click_and_drag() {
        use egui_kittest::kittest::Queryable;
        let mut h = dialog_harness(egui::vec2(1280.0, 800.0), "filter.blur.motionBlur");
        let angle = |h: &egui_kittest::Harness<'static, PhotospliceApp>| h.state().ui.dialogs.last().and_then(|d| d.fields["angle"].as_f64());
        let centre = |h: &egui_kittest::Harness<'static, PhotospliceApp>| h.get_by_role_and_label(egui::accesskit::Role::Slider, "Angle").rect().center();
        let press = |h: &mut egui_kittest::Harness<'static, PhotospliceApp>, pos: egui::Pos2, pressed: bool| {
            h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
        };
        assert_eq!(angle(&h), Some(0.0));
        // The dial's field is the dialog's first number, so it opens focused (#1757).
        let first = h.get_all_by_role(egui::accesskit::Role::SpinButton).next().unwrap();
        assert!(first.is_focused() && first.rect().center().y < h.get_by_label("Distance").rect().center().y, "the Angle field has focus");
        // Click above the centre: straight up is 90°.
        let up = centre(&h) + egui::vec2(0.0, -15.0);
        h.event(egui::Event::PointerMoved(up));
        press(&mut h, up, true);
        h.step();
        press(&mut h, up, false);
        h.run_steps(3);
        assert_eq!(angle(&h), Some(90.0));
        // Drag from the right (0°) round to the lower left (-135°).
        let c = centre(&h);
        let from = c + egui::vec2(15.0, 0.0);
        h.event(egui::Event::PointerMoved(from));
        press(&mut h, from, true);
        h.step();
        for (k, to) in [egui::vec2(12.0, 12.0), egui::vec2(0.0, 15.0), egui::vec2(-12.0, 12.0)].into_iter().enumerate() {
            h.event(egui::Event::PointerMoved(c + to));
            h.step();
            assert_eq!(angle(&h), Some([-45.0, -90.0, -135.0][k]), "dragged to {to:?}");
        }
        press(&mut h, c + egui::vec2(-12.0, 12.0), false);
        h.run_steps(3);
        assert_eq!(angle(&h), Some(-135.0));
        // A remembered or typed angle past half a turn is kept, not clamped to the dial's ±180°.
        if let Some(d) = h.state_mut().ui.dialogs.last_mut() {
            d.fields.insert("angle".into(), json!(270.0));
        }
        h.run_steps(3);
        assert_eq!(angle(&h), Some(270.0));
    }

    #[test]
    fn a_tall_filter_dialog_keeps_its_title_and_buttons_in_a_small_window() {
        // Filter › Render › Flame was taller than a 1000×560 window: OK and Cancel were cut off.
        use egui_kittest::kittest::Queryable;
        let size = egui::vec2(1000.0, 560.0);
        let h = dialog_harness(size, "filter.render.flame");
        for label in ["Flame", "OK", "Cancel"] {
            let r = h.get_by_label(label).rect();
            assert!(r.top() >= 0.0 && r.bottom() <= size.y, "{label} at {r:?} is outside the {size:?} window");
        }
    }

    #[test]
    fn a_moved_dialog_reopens_where_it_was_left() {
        use egui_kittest::kittest::Queryable;
        let size = egui::vec2(1280.0, 800.0);
        let mut h = dialog_harness(size, "filter.blur.gaussianBlur");
        let centred = h.get_by_label("Gaussian Blur").rect();
        // Drag the title bar up and to the left, staying inside the window: the dialog's square
        // preview makes it tall, so it opens only ~60 points below the top bound.
        let (from, by) = (centred.center(), egui::vec2(-240.0, -40.0));
        h.event(egui::Event::PointerMoved(from));
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        for k in 1..=6 {
            h.event(egui::Event::PointerMoved(from + by * (k as f32 / 6.0)));
            h.step();
        }
        h.event(egui::Event::PointerButton { pos: from + by, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        h.run_steps(3);
        let moved = h.get_by_label("Gaussian Blur").rect();
        assert!((moved.min - (centred.min + by)).length() < 2.0, "dragged: {centred:?} → {moved:?}");
        h.get_by_label("Cancel").click();
        h.run_steps(3);
        assert!(h.state().ui.dialogs.is_empty());
        // Reopened: where it was left.
        crate::filter_dialog::open(h.state_mut(), "filter.blur.gaussianBlur").unwrap();
        h.run_steps(4);
        let reopened = h.get_by_label("Gaussian Blur").rect();
        assert!((reopened.min - moved.min).length() < 2.0, "reopened at {reopened:?}, left at {moved:?}");
        h.get_by_label("Cancel").click();
        h.run_steps(3);
        // Another dialog keeps its own place: never moved, it opens centred.
        crate::filter_dialog::open(h.state_mut(), "filter.blur.motionBlur").unwrap();
        h.run_steps(4);
        let other = h.get_by_label("Motion Blur").rect();
        assert!(other.min.x > moved.min.x + 100.0, "Motion Blur opened centred: {other:?}");
    }

    /// #1757: like Photoshop, a value dialog opens on its first number with the text selected, so
    /// typing replaces it and Enter applies it; it reopens with the value applied last time, and a
    /// cancelled edit is not remembered.
    #[test]
    fn a_value_dialog_opens_on_its_first_number_selected_and_remembers_it() {
        use egui_kittest::kittest::Queryable;
        const BLUR: &str = "filter.blur.gaussianBlur";
        let mut h = dialog_harness(egui::vec2(1280.0, 800.0), BLUR);
        let radius = |h: &egui_kittest::Harness<'static, PhotospliceApp>| h.state().ui.dialogs.last().and_then(|d| d.fields["radius"].as_f64());
        assert_eq!(radius(&h), Some(1.0), "the default the first time");
        assert!(h.get_by_role(egui::accesskit::Role::SpinButton).is_focused(), "the Radius field has focus");
        h.event(egui::Event::Text("5".into()));
        h.run_steps(1);
        assert_eq!(radius(&h), Some(5.0), "typing replaced the selected 1");
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert!(h.state().ui.dialogs.is_empty(), "Enter is OK");
        assert_eq!(h.state().session.journal.last(), Some(&(BLUR.to_string(), json!({"radius": 5.0}))));

        crate::filter_dialog::open(h.state_mut(), BLUR).unwrap();
        h.run_steps(4);
        assert_eq!(radius(&h), Some(5.0), "reopens with the last value");
        assert!(h.get_by_role(egui::accesskit::Role::SpinButton).is_focused(), "focused again");
        h.event(egui::Event::Text("9".into()));
        h.run_steps(1);
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert!(h.state().ui.dialogs.is_empty(), "Esc is Cancel");
        crate::filter_dialog::open(h.state_mut(), BLUR).unwrap();
        h.run_steps(2);
        assert_eq!(radius(&h), Some(5.0), "a cancelled 9 is not remembered");
    }

    /// #4673: File › Automate › Fit Image holds two numbers and a check box, so its window is as
    /// wide as that content and its buttons, not the 380-point floor of the larger dialogs.
    #[test]
    fn fit_image_is_as_narrow_as_its_content() {
        use egui_kittest::kittest::Queryable;
        let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 64, "height": 64})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1280.0, 800.0)).build_ui_state(|ui, app| show(app, ui.ctx()), app);
        PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        let ctx = h.ctx.clone();
        let d = crate::menus::invoke(h.state_mut(), &ctx, "file.automate.fitImage", json!({})).unwrap()["dialog"].as_u64().unwrap();
        h.run_steps(4);
        let window = h.ctx.memory(|m| m.area_rect(egui::Id::new(("dialog", d)))).expect("the Fit Image window");
        assert!(window.width() < 300.0, "Fit Image is {} points wide", window.width());
        for label in ["Fit Image", "Width", "Height", "OK", "Cancel"] {
            assert!(window.contains_rect(h.get_by_label(label).rect()), "{label} inside the window");
        }
    }
}
