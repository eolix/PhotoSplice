//! Deflates the bundled UI fonts (`assets/fonts/*.ttf`, `fonts::BUNDLED`) into
//! `OUT_DIR/fonts/<file>.deflate`: 0.73 MB of binary instead of 1.5 MB, for the web build's size
//! gate (`packaging/web/package.sh`).
use std::io::Write as _;
use std::path::PathBuf;

fn main() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("fonts");
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&out)?;
        for name in ["Inter-Regular.ttf", "Inter-Medium.ttf", "Inter-SemiBold.ttf", "JetBrainsMono-Regular.ttf"] {
            let path = src.join(name);
            println!("cargo::rerun-if-changed={}", path.display());
            let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
            z.write_all(&std::fs::read(&path)?)?;
            std::fs::write(out.join(format!("{name}.deflate")), z.finish()?)?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        println!("cargo::error=deflating the bundled fonts in {}: {e}", src.display());
    }
}
