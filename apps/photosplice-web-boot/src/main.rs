//! Starts Photosplice in the browser from compressed parts of its WebAssembly.
//!
//! Static hosts cap the size of one file (Cloudflare Pages: 25 MiB), and the app's wasm is at
//! that size. `packaging/web/package.sh` gzips it and cuts it into parts listed in
//! `app/parts.txt`. This small module is what the packaged page loads: it fetches the parts in
//! order, joins and inflates them as they arrive, and hands the stream to the app's own generated
//! loader, so the browser still compiles while downloading. No file on the host is larger than
//! one part, however large the app grows.
//!
//! The browser inflates the parts (`DecompressionStream`: Safari 16.4, Firefox 113, Chrome 80).
//! In one that can't, this module inflates them itself, a little slower.
//!
//! A download that fails is tried again, the later tries past the browser's cache (a bad copy
//! there would otherwise fail forever). When every try fails the page says so and offers a reload.
//!
//! `trunk build` in `apps/photosplice-web` still gives the app as one `.wasm`, for development and
//! for hosts without a per-file cap; only the packaged site starts through this module.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// First line of `parts.txt`. Some hosts answer a missing file with the home page and HTTP 200;
/// without this an HTML page would be read as a list of parts.
#[cfg(any(target_arch = "wasm32", test))]
const MARKER: &str = "photosplice-parts 1";

/// The part names in a `parts.txt`, in order, or `None` when the text isn't one. A name is a
/// file next to `parts.txt`: anything that could point elsewhere is refused.
#[cfg(any(target_arch = "wasm32", test))]
fn parts(list: &str) -> Option<Vec<String>> {
    let mut lines = list.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next() != Some(MARKER) {
        return None;
    }
    let parts: Vec<String> = lines.map(str::to_owned).collect();
    let plain = |p: &String| !p.starts_with('.') && !p.contains(['/', '\\', '?', '#', ':', '%']);
    (!parts.is_empty() && parts.iter().all(plain)).then_some(parts)
}

/// Gzip inflated a chunk at a time, for browsers that can't do it themselves.
#[cfg(any(target_arch = "wasm32", test))]
struct Inflater(flate2::write::GzDecoder<Vec<u8>>);

#[cfg(any(target_arch = "wasm32", test))]
impl Inflater {
    fn new() -> Self {
        Inflater(flate2::write::GzDecoder::new(Vec::new()))
    }

    /// What the next `chunk` of the gzip stream inflates to (often nothing: the decoder holds on
    /// to input until it has a whole block).
    fn push(&mut self, chunk: &[u8]) -> std::io::Result<Vec<u8>> {
        std::io::Write::write_all(&mut self.0, chunk)?;
        Ok(std::mem::take(self.0.get_mut()))
    }

    /// The rest, once the stream has ended. An error when it ended early or its checksum is
    /// wrong, the same as the browser's inflater gives.
    fn finish(&mut self) -> std::io::Result<Vec<u8>> {
        self.0.try_finish()?;
        Ok(std::mem::take(self.0.get_mut()))
    }
}

#[cfg(target_arch = "wasm32")]
mod boot {
    use std::cell::RefCell;
    use std::rc::Rc;

    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::{JsFuture, spawn_local};
    use web_sys::{
        Headers, ReadableStream, ReadableWritablePair, RequestCache, RequestInit, Response, ResponseInit, StreamPipeOptions, TransformStream,
        TransformStreamDefaultController, Transformer, WritableStream,
    };

    use super::Inflater;

    /// Where packaging puts the app, next to this module's own files.
    const PARTS: &str = "./app/parts.txt";
    /// Seconds to wait before each further try; one more than this many tries are made.
    const RETRY_AFTER: [u32; 2] = [1, 3];

    // The app's `wasm-bindgen` loader. Its default export instantiates the module it is given and
    // runs the app's `main`. The path is fixed at compile time, so packaging gives the file this
    // name (Trunk's own name for it carries a content hash).
    #[wasm_bindgen(raw_module = "./app/photosplice-web.js")]
    extern "C" {
        #[wasm_bindgen(js_name = default, catch)]
        async fn start_app(module: JsValue) -> Result<JsValue, JsValue>;
    }

    // The browser's own inflater. `web-sys` has it only behind its unstable-API flag, which would
    // have to be set for the whole build, so the three members used are declared here.
    #[wasm_bindgen]
    extern "C" {
        type DecompressionStream;
        #[wasm_bindgen(constructor, catch)]
        fn new(format: &str) -> Result<DecompressionStream, JsValue>;
        #[wasm_bindgen(method, getter)]
        fn readable(this: &DecompressionStream) -> ReadableStream;
        #[wasm_bindgen(method, getter)]
        fn writable(this: &DecompressionStream) -> WritableStream;
    }

    fn window() -> Result<web_sys::Window, JsValue> {
        web_sys::window().ok_or_else(|| JsValue::from_str("no window"))
    }

    /// `fresh` skips the browser's cache.
    async fn fetch(url: &str, fresh: bool) -> Result<Response, JsValue> {
        let init = RequestInit::new();
        if fresh {
            init.set_cache(RequestCache::Reload);
        }
        let response: Response = JsFuture::from(window()?.fetch_with_str_and_init(url, &init)).await?.dyn_into()?;
        if !response.ok() {
            return Err(JsValue::from_str(&format!("{url}: HTTP {}", response.status())));
        }
        if response.headers().get("Content-Type")?.is_some_and(|t| t.starts_with("text/html")) {
            return Err(JsValue::from_str(&format!("{url}: the server sent a web page, not the file")));
        }
        Ok(response)
    }

    /// Copies each part into `sink` in order, then closes it.
    async fn feed(parts: Vec<String>, sink: WritableStream, fresh: bool) -> Result<(), JsValue> {
        let keep_open = StreamPipeOptions::new();
        keep_open.set_prevent_close(true);
        for part in &parts {
            let body = fetch(&format!("./app/{part}"), fresh).await?.body().ok_or_else(|| JsValue::from_str("part without a body"))?;
            JsFuture::from(body.pipe_to_with_options(&sink, &keep_open)).await?;
        }
        JsFuture::from(sink.close()).await?;
        Ok(())
    }

    /// Passes inflated bytes on to the stream's reader.
    fn emit(inflated: std::io::Result<Vec<u8>>, out: &TransformStreamDefaultController) -> Result<(), JsValue> {
        let inflated = inflated.map_err(|e| JsValue::from_str(&format!("the download is damaged ({e})")))?;
        if inflated.is_empty() { Ok(()) } else { out.enqueue_with_chunk(&js_sys::Uint8Array::from(inflated.as_slice())) }
    }

    /// A gzip inflater with the shape of `DecompressionStream`, running in this module.
    fn own_inflater() -> Result<TransformStream, JsValue> {
        let inflater = Rc::new(RefCell::new(Inflater::new()));
        let last = inflater.clone();
        let busy = || JsValue::from_str("the inflater is in use");
        let transform = Closure::<dyn FnMut(JsValue, TransformStreamDefaultController) -> Result<(), JsValue>>::new(
            move |chunk: JsValue, out: TransformStreamDefaultController| {
                let chunk = js_sys::Uint8Array::new(&chunk).to_vec();
                emit(inflater.try_borrow_mut().map_err(|_| busy())?.push(&chunk), &out)
            },
        );
        let flush = Closure::<dyn FnMut(TransformStreamDefaultController) -> Result<(), JsValue>>::new(move |out: TransformStreamDefaultController| {
            emit(last.try_borrow_mut().map_err(|_| busy())?.finish(), &out)
        });
        let transformer = Transformer::new();
        transformer.set_transform(&transform.into_js_value().unchecked_into());
        transformer.set_flush(&flush.into_js_value().unchecked_into());
        TransformStream::new_with_transformer(&transformer)
    }

    /// `compressed` inflated: by the browser where it can, otherwise here.
    fn inflated(compressed: &ReadableStream) -> Result<ReadableStream, JsValue> {
        let pair = if js_sys::Reflect::has(&js_sys::global(), &JsValue::from_str("DecompressionStream"))? {
            let inflater = DecompressionStream::new("gzip")?;
            ReadableWritablePair::new(&inflater.readable(), &inflater.writable())
        } else {
            let inflater = own_inflater()?;
            ReadableWritablePair::new(&inflater.readable(), &inflater.writable())
        };
        Ok(compressed.pipe_through(&pair))
    }

    async fn run(fresh: bool) -> Result<(), JsValue> {
        let list = JsFuture::from(fetch(PARTS, fresh).await?.text()?).await?.as_string().unwrap_or_default();
        let parts = super::parts(&list).ok_or_else(|| JsValue::from_str("app/parts.txt is not a parts list"))?;
        let joined = TransformStream::new()?;
        let wasm = inflated(&joined.readable())?;
        // Why the download stopped: the app's loader only reports that its stream broke.
        let cause = Rc::new(RefCell::new(None));
        let failed = cause.clone();
        let sink = joined.writable();
        spawn_local(async move {
            if let Err(e) = feed(parts, sink.clone(), fresh).await {
                let _ = sink.abort_with_reason(&e);
                failed.replace(Some(e));
            }
        });
        let headers = Headers::new()?;
        headers.set("Content-Type", "application/wasm")?;
        let init = ResponseInit::new();
        init.set_headers(&headers);
        let response = Response::new_with_opt_readable_stream_and_init(Some(&wasm), &init)?;
        let arg = js_sys::Object::new();
        js_sys::Reflect::set(&arg, &JsValue::from_str("module_or_path"), &response)?;
        match start_app(arg.into()).await {
            Ok(_) => Ok(()),
            Err(e) => Err(cause.take().unwrap_or(e)),
        }
    }

    async fn sleep(seconds: u32) {
        let wait = js_sys::Promise::new(&mut |done, _| {
            let ms = i32::try_from(seconds.saturating_mul(1000)).unwrap_or(i32::MAX);
            if window().and_then(|w| w.set_timeout_with_callback_and_timeout_and_arguments_0(&done, ms)).is_err() {
                let _ = done.call0(&JsValue::NULL);
            }
        });
        let _ = JsFuture::from(wait).await;
    }

    /// Replaces the page's "Loading" text. `reload` adds a link that reloads the page.
    fn say(text: &str, reload: bool) {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
        let Some(loading) = document.get_element_by_id("photosplice_loading") else { return };
        loading.set_text_content(None);
        let Ok(line) = document.create_element("p") else { return };
        line.set_text_content(Some(text));
        if reload && let Ok(link) = document.create_element("a") {
            link.set_text_content(Some("Reload"));
            let _ = link.set_attribute("href", "");
            let _ = line.append_child(&link);
        }
        let _ = loading.append_child(&line);
    }

    pub fn start() {
        spawn_local(async {
            let mut attempt = 0;
            loop {
                let error = match run(attempt > 0).await {
                    Ok(()) => return,
                    Err(e) => e,
                };
                web_sys::console::error_2(&JsValue::from_str("photosplice-web-boot: could not start PhotoSplice:"), &error);
                let Some(&wait) = RETRY_AFTER.get(attempt) else {
                    say("PhotoSplice couldn't be downloaded. Check your connection, then try again: ", true);
                    return;
                };
                say("Loading PhotoSplice\u{2026} (the download failed, trying again)", false);
                sleep(wait).await;
                attempt += 1;
            }
        });
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    boot::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("photosplice-web-boot only runs in the browser: packaging/web/package.sh builds it with `trunk build --release` in apps/photosplice-web-boot");
}

#[cfg(test)]
mod tests {
    use super::{Inflater, MARKER, parts};

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut z, bytes).expect("gzip");
        z.finish().expect("gzip")
    }

    /// Bytes that don't compress to nothing, so the gzip stream spans many chunks.
    fn sample() -> Vec<u8> {
        let mut x = 0x2545_f491_4f6c_dd1d_u64;
        (0..200_000)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                if i % 3 == 0 { x as u8 } else { (i / 7) as u8 }
            })
            .collect()
    }

    /// The parts arrive in whatever pieces the network gives; the inflater must not care where
    /// they are cut.
    #[test]
    fn the_inflater_gives_back_the_bytes_however_the_stream_is_cut() {
        let (plain, packed) = (sample(), gzip(&sample()));
        for cut in [1, 7, 4096, 65_536, packed.len()] {
            let mut inflater = Inflater::new();
            let mut out = Vec::new();
            for chunk in packed.chunks(cut) {
                out.extend(inflater.push(chunk).expect("push"));
            }
            out.extend(inflater.finish().expect("finish"));
            assert_eq!(out, plain, "chunks of {cut}");
        }
    }

    /// A download that stops early, or whose bytes changed on the way, must fail (the boot module
    /// then tries again), never start a damaged app.
    #[test]
    fn the_inflater_refuses_a_short_or_damaged_stream() {
        let packed = gzip(&sample());
        let run = |bytes: &[u8]| -> std::io::Result<Vec<u8>> {
            let mut inflater = Inflater::new();
            let mut out = inflater.push(bytes)?;
            out.extend(inflater.finish()?);
            Ok(out)
        };
        assert!(run(&packed).is_ok());
        assert!(run(&packed[..packed.len() / 2]).is_err(), "cut short");
        assert!(run(&packed[..packed.len() - 4]).is_err(), "trailer cut");
        let mut damaged = packed.clone();
        let middle = damaged.len() / 2;
        damaged[middle] ^= 0x55;
        assert!(run(&damaged).is_err(), "a changed byte");
        assert!(run(b"<!DOCTYPE html><html>not gzip</html>").is_err(), "a web page");
        assert!(run(b"").is_err(), "nothing at all");
    }

    #[test]
    fn a_parts_list_gives_its_names_in_order() {
        let list = format!("{MARKER}\nphotosplice-web-0123abcd.part-aa\r\n\n  photosplice-web-0123abcd.part-ab  \n");
        assert_eq!(parts(&list), Some(vec!["photosplice-web-0123abcd.part-aa".to_owned(), "photosplice-web-0123abcd.part-ab".to_owned()]));
    }

    /// A host that answers a missing file with its home page must not have that page read as a
    /// list of parts.
    #[test]
    fn a_web_page_is_not_a_parts_list() {
        assert_eq!(parts(include_str!("../index.html")), None);
        assert_eq!(parts(""), None);
        assert_eq!(parts("photosplice-web.part-aa\n"), None, "no marker");
        assert_eq!(parts(&format!("{MARKER}\n")), None, "no parts");
        assert_eq!(parts("photosplice-parts 2\na.part-aa\n"), None, "a later layout this build doesn't know");
    }

    /// Part names are joined onto `./app/`, so one that could leave that folder, or name another
    /// origin, refuses the whole list.
    #[test]
    fn a_part_name_cannot_point_outside_the_app_folder() {
        for bad in ["../index.html", "a/b.part-aa", "a\\b", "https://example.com/x", "//example.com/x", "x?y", "x#y", ".hidden", "%2e%2e"] {
            assert_eq!(parts(&format!("{MARKER}\ngood.part-aa\n{bad}\n")), None, "{bad}");
        }
    }

    /// The packaged site shows this crate's page, the development build the app's own. They are
    /// the same page apart from which program the `<link data-trunk>` line builds.
    #[test]
    fn the_page_is_the_apps_page() {
        let trunk = |html: &str| html.lines().filter(|l| !l.contains("data-trunk")).collect::<Vec<_>>().join("\n");
        let (boot, app) = (include_str!("../index.html"), include_str!("../../photosplice-web/index.html"));
        assert_eq!(trunk(boot), trunk(app));
        assert!(boot.contains(r#"data-bin="photosplice-web-boot""#) && boot.contains(r#"data-cargo-profile="wasm-release""#), "{boot}");
        assert!(boot.contains(r#"id="photosplice_loading""#), "the element the failure message replaces");
    }
}
