//! Preferences › Performance › Low Resolution Previews: on, live previews of a large document
//! render on a reduced copy; off (PhotoSplice's default, and the preference is hidden) they render
//! at full resolution. Filters always preview at full size, so a preview is what OK applies.
//!
//! A 3000 × 2000 document of 1 px black and white columns: the reduced copy (every 2nd pixel at
//! this size) is all black, so its preview shows a flat colour where the full-size one shows the
//! columns at 100 % (and their mipmapped grey zoomed out). Drives the real app offscreen on wgpu;
//! skips when no GPU adapter exists (like `drag_preview_canvas.rs`).

use photosplice_color::{Color, ColorMode, SampleType};
use photosplice_doc::{Document, Size};
use photosplice_geom::Rect;
use photosplice_ui_egui::PhotospliceApp;
use photosplice_ui_egui::control::{ControlRequest, handle};
use serde_json::{Value, json};

type Harness = egui_kittest::Harness<'static, PhotospliceApp>;

const W: i32 = 3000;
const H: i32 = 2000;

fn harness() -> Option<Harness> {
    let built = std::panic::catch_unwind(|| {
        egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 760.0)).with_pixels_per_point(1.0).with_max_steps(64).wgpu().build_eframe(|cc| {
            PhotospliceApp::setup_context(&cc.egui_ctx, Default::default());
            let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), Default::default());
            if let Some(rs) = cc.wgpu_render_state.as_ref() {
                app.set_wgpu(rs.clone());
            }
            app
        })
    });
    match built {
        Ok(h) => Some(h),
        Err(_) => {
            eprintln!("skipping: no GPU adapter");
            None
        }
    }
}

fn control(h: &mut Harness, method: &str, params: Value) {
    let ctx = h.ctx.clone();
    let (req, _rx) = ControlRequest::new(method, params);
    handle(h.state_mut(), &ctx, &req);
    h.run_steps(3);
}

fn columns_document() -> Document {
    let mut doc = Document::with_background("columns", Size::new(W as u32, H as u32), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let s = doc.layers[0].surface_mut().expect("background pixels");
    for x in (0..W).step_by(2) {
        s.fill_rect(Rect::new(x, 0, x + 1, H), &[0.0, 0.0, 0.0, 1.0]);
    }
    doc
}

fn setup(h: &mut Harness, zoom: f32) {
    {
        let app = h.state_mut();
        app.session.add_document(columns_document(), None);
        app.sync_views();
        app.ui.extras.rulers = false;
    }
    control(h, "ui.set", json!({"zoom": zoom, "center": [W / 2, H / 2]}));
    h.run_steps(4);
}

/// Whether the canvas draws on the GPU (the previews under test are the GPU canvas's).
fn on_gpu(h: &mut Harness) -> bool {
    let _ = h.render();
    let gpu = h.state().perf.gpu;
    if !gpu {
        eprintln!("skipping: the canvas isn't on the GPU");
    }
    gpu
}

fn low_res(h: &mut Harness, on: bool) {
    h.state_mut().run("prefs.set", json!({"path": "performance.lowResolutionPreviews", "value": on})).expect("prefs.set");
    h.run_steps(4);
}

/// Luma along a 150 px row near the canvas's top-left corner, clear of centred dialogs (the
/// filter dialogs' larger previews reach to within ~300 px of the canvas's left edge).
fn row(h: &mut Harness) -> Vec<i32> {
    h.run_steps(4);
    let canvas = {
        let app = h.state();
        photosplice_ui_egui::rulers::content_rect(app, app.last_canvas_rect)
    };
    // egui_kittest waits 10 s for the GPU. Windows CI renders on WARP, a software rasterizer,
    // where the first full-resolution composite of this 6 MP document can take longer; the
    // submitted work still finishes, so wait for it again instead of failing.
    let img = (0..3).find_map(|_| h.render().ok()).expect("render");
    let (x0, y) = (canvas.min.x as u32 + 20, canvas.min.y as u32 + 40);
    (x0..x0 + 150)
        .map(|x| {
            let p = img.get_pixel(x, y).0;
            (i32::from(p[0]) * 3 + i32::from(p[1]) * 6 + i32::from(p[2])) / 10
        })
        .collect()
}

/// Fraction of neighbouring screen pixels that differ like black and white.
fn columns(r: &[i32]) -> f32 {
    r.windows(2).filter(|w| (w[0] - w[1]).abs() > 100).count() as f32 / (r.len() - 1) as f32
}

fn mean(r: &[i32]) -> f32 {
    r.iter().sum::<i32>() as f32 / r.len() as f32
}

/// Dragging an adjustment layer's control (Properties panel) previews through the reduced copy
/// only as coarse as the screen (#5764): at 100 % every column shows even with low-resolution
/// previews on; zoomed out to 50 % the drag runs on the reduced copy (only the black columns, inverted), and with
/// them off it matches the committed view (the columns' mipmapped grey).
#[test]
fn adjustment_layer_drag_previews_at_full_resolution_when_low_res_is_off() {
    let _gpu = photosplice_testkit::gpu_lock();
    let Some(mut h) = harness() else { return };
    setup(&mut h, 1.0);
    if !on_gpu(&mut h) {
        return;
    }
    assert!(columns(&row(&mut h)) > 0.9, "the canvas shows the 1 px columns at 100 %");
    low_res(&mut h, true);
    let layer = {
        let app = h.state_mut();
        app.run("layer.newAdjustmentLayer.invert", json!({})).expect("adjustment layer");
        let id = app.session.active().and_then(|d| d.active_layer).expect("active layer");
        // What the Properties panel sets while a slider drags.
        app.live_adjust = Some((id, json!({})));
        id
    };
    let at_100 = row(&mut h);
    assert!(columns(&at_100) > 0.9, "low-res on at 100 %: the drag is never coarser than the screen ({})", columns(&at_100));
    control(&mut h, "ui.set", json!({"zoom": 0.5, "center": [W / 2, H / 2]}));
    h.state_mut().live_adjust = None;
    let committed = mean(&row(&mut h));
    assert!(committed > 60.0, "zoomed out the inverted columns average to grey ({committed})");
    h.state_mut().live_adjust = Some((layer, json!({})));
    let reduced = mean(&row(&mut h));
    assert!(reduced > 225.0, "low-res on at 50 %: the reduced copy keeps only the (inverted) black columns ({reduced})");
    low_res(&mut h, false);
    h.state_mut().live_adjust = Some((layer, json!({})));
    let full = mean(&row(&mut h));
    assert!((full - committed).abs() < 8.0, "low-res previews off: the drag matches the committed view ({full} vs {committed})");
}

#[test]
fn filter_dialog_previews_at_full_resolution_when_low_res_is_off() {
    let _gpu = photosplice_testkit::gpu_lock();
    let Some(mut h) = harness() else { return };
    setup(&mut h, 1.0);
    if !on_gpu(&mut h) {
        return;
    }
    // Offset 0, 0 leaves the pixels as they are: any change is the preview's resolution.
    low_res(&mut h, true);
    photosplice_ui_egui::filter_dialog::open(h.state_mut(), "filter.other.offset").expect("Offset dialog");
    let on = row(&mut h);
    assert!(columns(&on) > 0.9, "low-res previews on: a filter still previews at full size ({})", columns(&on));
    low_res(&mut h, false);
    let full = row(&mut h);
    assert!(columns(&full) > 0.9, "low-res previews off: every column shows ({})", columns(&full));
}

#[test]
fn adjustment_dialog_zoomed_out_skips_the_reduced_copy_when_low_res_is_off() {
    let _gpu = photosplice_testkit::gpu_lock();
    let Some(mut h) = harness() else { return };
    setup(&mut h, 0.5);
    if !on_gpu(&mut h) {
        return;
    }
    let committed = mean(&row(&mut h));
    assert!(committed > 60.0, "zoomed out the columns average to grey ({committed})");
    low_res(&mut h, true);
    photosplice_ui_egui::adjust_dialog::open(h.state_mut(), "image.adjustments.levels").expect("Levels dialog");
    h.run_steps(4);
    assert!(photosplice_ui_egui::adjust_preview::gpu_proxy(h.state_mut(), 0, 0.5).is_some(), "low-res on: zoomed out on the reduced copy");
    let reduced = mean(&row(&mut h));
    assert!(reduced < 30.0, "the reduced copy keeps only the black columns ({reduced})");
    low_res(&mut h, false);
    assert!(photosplice_ui_egui::adjust_preview::gpu_proxy(h.state_mut(), 0, 0.5).is_none(), "off: no reduced copy");
    let full = mean(&row(&mut h));
    assert!((full - committed).abs() < 8.0, "off: the preview matches the committed view ({full} vs {committed})");
}
