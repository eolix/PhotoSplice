#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The complete import: native document mapping, or the preview fallback.
    if photosplice_affinity::is_affinity(data) {
        let _ = photosplice_io::import("fuzz.af", data);
    }
});
