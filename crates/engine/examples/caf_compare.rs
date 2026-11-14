//! Content-Aware Fill on real photos, for comparing with Reference's output.
//!
//! ```text
//! cargo run --release -p photosplice-engine --example caf_compare -- <cases.json> <images dir> <out dir> [params json]
//! ```
//!
//! `cases.json` maps a case name to `{"image": "<file in images dir>", "points": [[x, y], …]}`:
//! the lasso, in image pixels. Each case opens its image, selects the lasso (anti-aliased, as
//! Reference's scripted selections are) and runs `edit.contentAwareFill` with `params` (default:
//! the command's defaults), then writes the flattened result as `<out dir>/<case>.png` and prints
//! `<case> <ms>`. The images are public-domain photos, not committed.
use std::time::Instant;

use photosplice_engine::Session;
use serde_json::{Value, json};

type Res<T> = Result<T, String>;

fn run_case(name: &str, case: &Value, images: &std::path::Path, out: &std::path::Path, params: &Value) -> Res<f64> {
    let file = case.get("image").and_then(Value::as_str).ok_or(format!("{name}: no image"))?;
    let points = case.get("points").cloned().ok_or(format!("{name}: no points"))?;
    let bytes = std::fs::read(images.join(file)).map_err(|e| format!("{file}: {e}"))?;
    let doc = photosplice_io::import(file, &bytes).map_err(|e| format!("{file}: {e}"))?.document;
    let mut s = Session::new();
    s.open_document(doc, Some(file.into()));
    s.execute("select.lasso", json!({"points": points, "mode": "replace"})).map_err(|e| format!("{name}: select.lasso: {e}"))?;
    let t = Instant::now();
    s.execute("edit.contentAwareFill", params.clone()).map_err(|e| format!("{name}: edit.contentAwareFill: {e}"))?;
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let st = s.active().ok_or(format!("{name}: no document"))?;
    let png = photosplice_io::export(&st.doc, "png", &Default::default()).map_err(|e| format!("{name}: export: {e}"))?;
    std::fs::write(out.join(format!("{name}.png")), png.bytes).map_err(|e| format!("{name}: {e}"))?;
    Ok(ms)
}

fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, cases, images, out, rest @ ..] = args.as_slice() else {
        return Err("usage: caf_compare <cases.json> <images dir> <out dir> [params json]".into());
    };
    let params: Value = match rest.first() {
        Some(p) => serde_json::from_str(p).map_err(|e| format!("params: {e}"))?,
        None => json!({}),
    };
    let cases: serde_json::Map<String, Value> =
        serde_json::from_str(&std::fs::read_to_string(cases).map_err(|e| format!("{cases}: {e}"))?).map_err(|e| format!("{cases}: {e}"))?;
    let (images, out) = (std::path::Path::new(images), std::path::Path::new(out));
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    for (name, case) in &cases {
        match run_case(name, case, images, out, &params) {
            Ok(ms) => println!("{name} {ms:.0}"),
            Err(e) => println!("{name} error: {e}"),
        }
    }
    Ok(())
}
