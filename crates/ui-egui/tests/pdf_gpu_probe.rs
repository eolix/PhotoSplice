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

/// A stroked rectangle (a PDF table cell's border) draws the same on the GPU as on the CPU.
#[test]
fn a_stroked_cell_border_draws_alike_on_the_gpu() {
    let content = "0.75 0 0 -0.75 0 100 cm q 2 2 260 130 re W* n q 0 G 0 J 0 j 10 M 2 w 20 20 m 120 20 l 120 60 l 20 60 l h S Q Q";
    let pdf = format!(
        "%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length {} >>\nstream\n{content}\nendstream\nendobj\n",
        content.len()
    );
    let mut bytes = pdf.into_bytes();
    // The xref is rebuilt by the reader's repair when absent.
    bytes.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
    let doc = photosplice_io::import("cell.pdf", &bytes).unwrap().document;
    let instance = wgpu::Instance::default();
    let Ok(adapter) = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else { return };
    let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let mut comp = photosplice_gpu::Compositor::try_new_with_format(&device, photosplice_gpu::Compositor::preferred_acc_format(&adapter)).unwrap();
    let (gpu, _) = photosplice_gpu::render_to_vec_stats(&mut comp, &device, &queue, &doc, doc.bounds()).unwrap();
    let cpu = photosplice_compose::flatten(&doc);
    let w = doc.size.width as usize;
    // 144 ppi: the left edge at x = 30, a 3 px line.
    let y = 60;
    let a = |v: &[[f32; 4]], x: usize| v[y * w + x][3];
    let cpu_row: Vec<f32> = (26..35).map(|x| a(&cpu.px, x)).collect();
    let gpu_row: Vec<f32> = (26..35).map(|x| a(&gpu, x)).collect();
    eprintln!("size {}x{}\ncpu {cpu_row:?}\ngpu {gpu_row:?}", doc.size.width, doc.size.height);
    for (c, g) in cpu_row.iter().zip(&gpu_row) {
        assert!((c - g).abs() < 0.02, "cpu {cpu_row:?}\ngpu {gpu_row:?}");
    }
}
