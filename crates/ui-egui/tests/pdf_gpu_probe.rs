//! `PHOTOSPLICE_GPU_PROBE=<file>`: how long the GPU compositor takes over a whole document opened
//! from `file` (a PDF of many pages and layers, say), cold and warm, with its stats.
//!
//!     PHOTOSPLICE_GPU_PROBE=paper.pdf cargo test --profile devfast -p photosplice-ui-egui \
//!         --test pdf_gpu_probe -- --ignored --nocapture

use eframe::wgpu;

/// Polls a future that resolves without waiting (wgpu's adapter and device requests on native).
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

#[test]
#[ignore]
fn gpu_composite_of_a_file() {
    let Some(path) = std::env::var_os("PHOTOSPLICE_GPU_PROBE") else { return };
    let bytes = std::fs::read(&path).unwrap();
    let t = std::time::Instant::now();
    let doc = photosplice_io::import("probe.pdf", &bytes).unwrap().document;
    eprintln!("import {:?}: {} × {}, {} top-level layers", t.elapsed(), doc.size.width, doc.size.height, doc.layers.len());
    let instance = wgpu::Instance::default();
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).unwrap();
    let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut comp = photosplice_gpu::Compositor::try_new_with_format(&device, photosplice_gpu::Compositor::preferred_acc_format(&adapter)).unwrap();
    let t = std::time::Instant::now();
    let plan = photosplice_gpu::plan(&doc).unwrap();
    eprintln!("plan {:?}: {} passes", t.elapsed(), plan.passes.len());
    // `PHOTOSPLICE_GPU_PROBE_PLAN=1`: plan 20 times (for a sampling profiler), then stop.
    if std::env::var_os("PHOTOSPLICE_GPU_PROBE_PLAN").is_some() {
        for _ in 0..20 {
            let _ = photosplice_gpu::plan(&doc);
        }
        return;
    }
    for run in ["cold", "warm"] {
        let t = std::time::Instant::now();
        let (_, s) = photosplice_gpu::render_to_vec_stats(&mut comp, &device, &queue, &doc, doc.bounds()).unwrap();
        eprintln!("{run}: {:?} {s:?}", t.elapsed());
    }
}
