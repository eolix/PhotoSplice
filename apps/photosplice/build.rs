//! Windows only: embed the app icon and version info (VERSIONINFO) into `photosplice.exe`.
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile from macOS or Linux still links, unless `PHOTOSPLICE_REQUIRE_WINRES=1` (set by the
//! release workflow) turns it into an error.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/photosplice.ico");
    println!("cargo:rerun-if-env-changed=PHOTOSPLICE_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/photosplice.ico")
        .set("ProductName", "PhotoSplice")
        .set("FileDescription", "PhotoSplice image editor")
        .set("CompanyName", "PhotoSplice")
        .set("LegalCopyright", "Copyright (c) the PhotoSplice authors; ArtCraft Team and the PhotoCraft contributors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "photosplice.exe")
        .set("InternalName", "photosplice");
    if let Err(e) = res.compile() {
        if std::env::var_os("PHOTOSPLICE_REQUIRE_WINRES").is_some() {
            panic!("embedding Windows resources failed: {e}");
        }
        println!("cargo:warning=photosplice.exe built without icon/version resources: {e}");
    }
}
