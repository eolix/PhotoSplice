//! The renderer of [hayro](https://github.com/LaurenzV/hayro) 0.8.0 (`hayro/src`, © Laurenz
//! Stampfl, Apache-2.0 OR MIT, licence texts beside this file), vendored so the PDF importer can
//! draw a chosen subset of a page's operations into a pixel layer (hayro keeps its `Renderer`
//! private). Changes from upstream: module paths (`crate::` → this module, `hayro::` re-exports
//! for `hayro_interpret`, `kurbo` and `vello_cpu`), the crate-level attributes and docs, and
//! [`Renderer`] / [`GlobalState`] made reachable from `pdf`, and [`render_into`] clipping to a
//! rectangle it is given (Import PDF's Crop To) instead of always the crop box, and
//! `mask::soft_mask_bytes`, a soft mask drawn as bytes for a layer mask, and [`tiling_tile`], one
//! tile of a tiling pattern for a pattern fill.
#![allow(clippy::all, clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable, clippy::indexing_slicing, missing_docs, dead_code)]



use fearless_simd::Level;
use hayro::hayro_interpret::Device;
use hayro::hayro_interpret::FillRule;
use hayro::hayro_interpret::InterpreterCache;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::font::GlyphRun;
use hayro::hayro_interpret::hayro_syntax::Pdf;
use hayro::hayro_interpret::hayro_syntax::page::Page;
use hayro::hayro_interpret::util::{RectExt, TransformExt};
use hayro::hayro_interpret::{BlendMode, Context, DrawMode, DrawProps, ImageDrawProps, SoftMask};
use hayro::hayro_interpret::{ClipPath, interpret_page};
use hayro::kurbo::{Affine, BezPath, Rect, Shape};
use pic_scale::Scaler;
use rustc_hash::FxHashMap;
use std::cell::RefCell;
use std::ops::RangeInclusive;
use std::rc::Rc;


use hayro::vello_cpu::color::palette::css::{TRANSPARENT, WHITE};
use hayro::vello_cpu::color::{AlphaColor, Srgb};
use hayro::vello_cpu::peniko::{Compose, Fill, Mix};
use hayro::vello_cpu::{Mask, Pixmap, RenderContext, peniko};

mod clip;
mod glyph;
mod image;
mod mask;
mod paint;
mod path;

pub(crate) use mask::soft_mask_bytes;

/// One tile of the tiling pattern `t` as straight RGBA8 pixels, `w` × `h`: `tile` maps pattern
/// space to the tile's pixels, and `steps` are the pattern's repeat in those pixels (content
/// reaching past the tile is drawn wrapped, from the neighbouring repeats).
pub(crate) fn tiling_tile(t: &hayro::hayro_interpret::pattern::TilingPattern<'_>, global: &GlobalState, tile: Affine, w: u16, h: u16, steps: [hayro::kurbo::Vec2; 2]) -> Vec<u8> {
    let mut ctx = hayro::vello_cpu::RenderContext::new(w, h);
    {
        let mut r = Renderer::new(&mut ctx, global);
        r.inside_pattern = true;
        for i in -1..=1 {
            for j in -1..=1 {
                let shift = Affine::translate(steps[0] * f64::from(i) + steps[1] * f64::from(j));
                let _ = t.interpret(&mut r, shift * tile, false);
            }
        }
    }
    ctx.flush();
    let mut pixmap = hayro::vello_cpu::Pixmap::new(w, h);
    ctx.render_with(
        &mut pixmap,
        &mut hayro::vello_cpu::Resources::default(),
        hayro::vello_cpu::RasterizerSettings { target_init: hayro::vello_cpu::TargetInit::Clear(hayro::vello_cpu::color::palette::css::TRANSPARENT), ..Default::default() },
    );
    let mut rgba = pixmap.data_as_u8_slice().to_vec();
    for q in rgba.chunks_exact_mut(4) {
        let a = u32::from(q[3]);
        if a > 0 && a < 255 {
            for c in &mut q[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    rgba
}

pub(crate) struct GlobalState {
    level: Level,
    outline_cache: Rc<RefCell<FxHashMap<u128, Rc<BezPath>>>>,
    scaler: Scaler,
    force_image_interpolation: bool,
}

impl GlobalState {
    pub(crate) fn new(cache: &RenderCache<'_>, settings: &RenderSettings) -> Self {
        Self {
            level: Level::new(),
            outline_cache: cache.outline_cache.clone(),
            scaler: Scaler::new(image::RESAMPLING_FUNCTION),
            force_image_interpolation: settings.force_image_interpolation,
        }
    }
}

pub(crate) struct Renderer<'a> {
    global: &'a GlobalState,
    pub(crate) ctx: &'a mut RenderContext,
    pub(crate) inside_pattern: bool,
    pub(crate) soft_mask_cache: FxHashMap<u128, Mask>,
    pub(crate) in_type3_glyph: bool,
}

impl<'r> Renderer<'r> {
    pub(crate) fn new(ctx: &'r mut RenderContext, global: &'r GlobalState) -> Self {
        Self {
            global,
            ctx,
            inside_pattern: false,
            soft_mask_cache: FxHashMap::default(),
            in_type3_glyph: false,
        }
    }

    fn child_context(&self, width: u16, height: u16) -> RenderContext {
        RenderContext::new_with(width, height, derive_settings(self.ctx.render_settings()))
    }

    fn apply_draw_props(&mut self, props: &DrawProps<'_>) {
        self.ctx.set_transform(props.transform);
        self.apply_soft_mask(props.soft_mask.as_ref());
        self.ctx
            .set_blend_mode(convert_blend_mode(props.blend_mode));
    }

    fn apply_image_props(&mut self, props: &ImageDrawProps<'_>) {
        self.ctx.set_transform(props.transform);
        self.apply_soft_mask(props.soft_mask.as_ref());
        self.ctx
            .set_blend_mode(convert_blend_mode(props.blend_mode));
    }
}

impl<'a, 'r> Device<'a> for Renderer<'r> {
    fn draw_image(&mut self, image: hayro::hayro_interpret::Image<'a, '_>, props: ImageDrawProps<'a>) {
        Self::draw_pdf_image(self, image, props);
    }

    fn push_clip_path(&mut self, clip_path: &ClipPath) {
        Self::push_clip_path(self, clip_path);
    }

    fn push_clip_rect(&mut self, rect: &Rect) {
        Self::push_clip_rect(self, rect);
    }

    fn push_transparency_group(
        &mut self,
        opacity: f32,
        mask: Option<SoftMask<'a>>,
        blend_mode: BlendMode,
    ) {
        Self::push_transparency_group(self, opacity, mask, blend_mode);
    }

    fn pop_clip(&mut self) {
        Self::pop_clip(self);
    }

    fn pop_transparency_group(&mut self) {
        Self::pop_transparency_group(self);
    }

    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, draw_mode: &DrawMode) {
        Self::draw_path(self, path, props, draw_mode);
    }

    fn draw_rect(&mut self, rect: &Rect, props: DrawProps<'a>, draw_mode: &DrawMode) {
        Self::draw_rect(self, rect, props, draw_mode);
    }

    fn draw_glyph_run(
        &mut self,
        glyph_run: &GlyphRun<'_, 'a>,
        props: DrawProps<'a>,
        draw_mode: &DrawMode,
    ) {
        Self::draw_glyph_run(self, glyph_run, props, draw_mode);
    }
}

/// A cache used by the renderer.
///
/// Ideally, such a cache should be constructed once per PDF and then reused across
/// multiple render invocations on the same document.
#[derive(Clone, Default)]
pub struct RenderCache<'a> {
    pub(crate) interpreter_cache: InterpreterCache<'a>,
    pub(crate) outline_cache: Rc<RefCell<FxHashMap<u128, Rc<BezPath>>>>,
}

impl<'a> RenderCache<'a> {
    /// Create a new render cache.
    pub fn new() -> Self {
        Self {
            interpreter_cache: InterpreterCache::new(),
            outline_cache: Rc::new(RefCell::new(FxHashMap::default())),
        }
    }
}

/// Settings for rendering a page.
#[derive(Clone, Copy, Default)]
pub struct RenderSettings {
    /// Whether all images should forcibly be rendered with bilinear interpolation.
    pub force_image_interpolation: bool,
}

/// Settings for the output pixmap.
#[derive(Clone, Copy)]
pub struct PixmapSettings {
    /// Horizontal scale factor.
    pub x_scale: f32,
    /// Vertical scale factor.
    pub y_scale: f32,
    /// Background color.
    pub bg_color: AlphaColor<Srgb>,
}

impl Default for PixmapSettings {
    fn default() -> Self {
        Self {
            x_scale: 1.0,
            y_scale: 1.0,
            bg_color: TRANSPARENT,
        }
    }
}

/// Render a page to a pixmap with the given settings.
///
/// This is a "simplified" method in case all you want to achieve is rendering a
/// page with a certain scale factor.
///
/// If you want to have more control over how exactly the page is rendered (e.g. with
/// an arbitrary transform or into a custom pixel buffer), use [`render_into`]
pub fn render<'a>(
    page: &'a Page<'a>,
    cache: &RenderCache<'a>,
    interpreter_settings: &InterpreterSettings,
    render_settings: &RenderSettings,
    pixmap_settings: &PixmapSettings,
) -> Pixmap {
    let (width, height) = page.render_dimensions();
    let mut ctx = RenderContext::new(
        (width * pixmap_settings.x_scale) as u16,
        (height * pixmap_settings.y_scale) as u16,
    );
    let transform = Affine::scale_non_uniform(
        pixmap_settings.x_scale as f64,
        pixmap_settings.y_scale as f64,
    ) * page.initial_transform(true).to_kurbo();
    render_into(
        page,
        cache,
        interpreter_settings,
        render_settings,
        &mut ctx,
        transform,
        page.intersected_crop_box().to_kurbo(),
    );
    ctx.flush();

    let mut pixmap = Pixmap::new(ctx.width(), ctx.height());
    ctx.render_with(
        &mut pixmap,
        &mut hayro::vello_cpu::Resources::default(),
        hayro::vello_cpu::RasterizerSettings {
            target_init: hayro::vello_cpu::TargetInit::Clear(pixmap_settings.bg_color),
            ..Default::default()
        },
    );
    pixmap
}

/// Render a page into the given [`RenderContext`] with the
/// given transform.
///
/// See the [following example](https://github.com/LaurenzV/hayro/blob/main/hayro/examples/render.rs)
/// if you are unsure how to call this method.
pub fn render_into<'a>(
    page: &'a Page<'a>,
    cache: &RenderCache<'a>,
    interpreter_settings: &InterpreterSettings,
    render_settings: &RenderSettings,
    ctx: &mut RenderContext,
    transform: Affine,
    clip: Rect,
) {
    let mut state = Context::new(
        transform,
        Rect::new(0.0, 0.0, ctx.width() as f64, ctx.height() as f64),
        &cache.interpreter_cache,
        page.xref(),
        interpreter_settings.clone(),
    );
    ctx.take_current_state();
    ctx.reset_mask();
    ctx.reset_filter_effect();

    let global = GlobalState::new(cache, render_settings);
    let mut device = Renderer::new(ctx, &global);
    let mut clip_path = clip.to_path(0.1);
    clip_path.apply_affine(transform);
    device.push_clip_path(&ClipPath {
        path: clip_path,
        fill: FillRule::NonZero,
    });

    interpret_page(page, &mut state, &mut device);

    device.pop_clip();
}

// Just a convenience method for testing.
#[doc(hidden)]
pub fn render_pdf(
    pdf: &Pdf,
    scale: f32,
    settings: InterpreterSettings,
    range: Option<RangeInclusive<usize>>,
) -> Option<Vec<Pixmap>> {
    let cache = RenderCache::new();
    let rendered = pdf
        .pages()
        .iter()
        .enumerate()
        .flat_map(|(idx, page)| {
            if range.clone().is_some_and(|range| !range.contains(&idx)) {
                return None;
            }

            let pixmap = render(
                page,
                &cache,
                &settings,
                &RenderSettings::default(),
                &PixmapSettings {
                    x_scale: scale,
                    y_scale: scale,
                    bg_color: WHITE,
                },
            );

            Some(pixmap)
        })
        .collect();

    Some(rendered)
}

pub(crate) fn derive_settings(settings: &hayro::vello_cpu::RenderSettings) -> hayro::vello_cpu::RenderSettings {
    hayro::vello_cpu::RenderSettings {
        num_threads: 0,
        ..*settings
    }
}

fn convert_fill_rule(fill_rule: FillRule) -> Fill {
    match fill_rule {
        FillRule::NonZero => Fill::NonZero,
        FillRule::EvenOdd => Fill::EvenOdd,
    }
}

fn convert_blend_mode(blend_mode: BlendMode) -> peniko::BlendMode {
    let mix = match blend_mode {
        BlendMode::Normal => Mix::Normal,
        BlendMode::Multiply => Mix::Multiply,
        BlendMode::Screen => Mix::Screen,
        BlendMode::Overlay => Mix::Overlay,
        BlendMode::Darken => Mix::Darken,
        BlendMode::Lighten => Mix::Lighten,
        BlendMode::ColorDodge => Mix::ColorDodge,
        BlendMode::ColorBurn => Mix::ColorBurn,
        BlendMode::HardLight => Mix::HardLight,
        BlendMode::SoftLight => Mix::SoftLight,
        BlendMode::Difference => Mix::Difference,
        BlendMode::Exclusion => Mix::Exclusion,
        BlendMode::Hue => Mix::Hue,
        BlendMode::Saturation => Mix::Saturation,
        BlendMode::Color => Mix::Color,
        BlendMode::Luminosity => Mix::Luminosity,
    };

    peniko::BlendMode::new(mix, Compose::SrcOver)
}
