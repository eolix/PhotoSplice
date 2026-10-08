//! What the app draws against Photoshop: renders a PSD through the GPU compositor (the app's
//! canvas) and the CPU compositor, and compares both with the file's own merged composite.
//!
//! ```sh
//! cargo run --release -p photosuite-io --example gpu_diff -- file.psd [out.png]
//! SCALE=4 …          # upscale the png: GPU | CPU | Photoshop | GPU-vs-Photoshop heat map
//! ```
//!
//! Skips (with a message) when no GPU adapter with 32-bit float targets exists.
fn main() {
    let path = std::env::args().nth(1).expect("usage: gpu_diff file.psd [out.png]");
    let bytes = std::fs::read(&path).expect("read");
    let file = photosuite_psd::PsdFile::from_bytes(&bytes).expect("parse");
    let doc = photosuite_io::import(&path, &bytes).expect("import").document;
    let merged = photosuite_io::merged_composite(&file).expect("merged composite");
    let cpu = photosuite_compose::flatten(&doc).px;

    let instance = wgpu::Instance::default();
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else {
        eprintln!("no GPU adapter");
        return;
    };
    let format = photosuite_gpu::Compositor::preferred_acc_format(&adapter);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
    let mut comp = photosuite_gpu::Compositor::try_new_with_format(&device, format).expect("compositor");
    let gpu = photosuite_gpu::render_to_vec(&mut comp, &device, &queue, &doc, doc.bounds()).expect("GPU render");

    let over = |p: [f32; 4]| [p[0] * p[3] + 1.0 - p[3], p[1] * p[3] + 1.0 - p[3], p[2] * p[3] + 1.0 - p[3]];
    let err = |a: &[[f32; 4]], b: &[[f32; 4]]| -> (f32, f32) {
        let ds: Vec<f32> = a.iter().zip(b).map(|(x, y)| (0..3).map(|c| (over(*x)[c] - over(*y)[c]).abs()).fold(0.0, f32::max)).collect();
        (ds.iter().sum::<f32>() / ds.len().max(1) as f32, ds.iter().copied().fold(0.0, f32::max))
    };
    let (gm, gx) = err(&gpu, &merged);
    let (cm, cx) = err(&cpu, &merged);
    let (dm, dx) = err(&gpu, &cpu);
    println!("{path} ({format:?})\n  GPU vs Photoshop: mean {gm:.4} max {gx:.3}\n  CPU vs Photoshop: mean {cm:.4} max {cx:.3}\n  GPU vs CPU:       mean {dm:.4} max {dx:.3}");

    let Some(out) = std::env::args().nth(2) else { return };
    let (w, h) = (doc.size.width as usize, doc.size.height as usize);
    let k: usize = std::env::var("SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1).clamp(1, 16);
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut img = vec![255u8; w * 4 * k * h * k * 4];
    for y in 0..h * k {
        for x in 0..w * 4 * k {
            let (panel, px, py) = (x / (w * k), (x % (w * k)) / k, y / k);
            let i = py * w + px;
            let rgb = match panel {
                0 => over(gpu[i]).map(q),
                1 => over(cpu[i]).map(q),
                2 => over(merged[i]).map(q),
                _ => {
                    let (a, b) = (over(gpu[i]), over(merged[i]));
                    let d = (0..3).map(|c| (a[c] - b[c]).abs()).fold(0.0f32, f32::max);
                    [q((d * 4.0).min(1.0)), q((1.0 - d * 4.0).max(0.0) * 0.3), 0]
                }
            };
            let o = (y * w * 4 * k + x) * 4;
            img[o..o + 3].copy_from_slice(&rgb);
        }
    }
    let image = photosuite_codecs::Image::from_raw((w * 4 * k) as u32, (h * k) as u32, photosuite_codecs::ChannelLayout::Rgba, photosuite_codecs::SampleType::U8, img)
        .expect("image");
    std::fs::write(&out, photosuite_codecs::encode(&image, photosuite_codecs::Format::Png, &Default::default()).expect("png")).expect("write");
    println!("wrote {out}");
}
