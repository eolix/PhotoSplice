//! Each kind of place that follows a path stored in a document goes through [`super::resolve`]: a
//! link to another computer is never followed, and a relative link next to the document is.

use std::path::{Path, PathBuf};

use photosplice_doc::{LayerContent, LayerId, SmartSource};
use photosplice_geom::Rect;
use serde_json::json;

use crate::Session;

/// A link to a share on a TEST-NET-1 address, which never answers.
const REMOTE: &str = r"\\192.0.2.1\share\x.png";

/// An empty folder of its own (tests run in parallel).
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("photosplice-doc-path-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A 32×32 PNG of one colour.
fn png(path: &Path, rgb: [u8; 3]) {
    let px = rgb.repeat(32 * 32);
    let image = photosplice_codecs::Image::from_u8(32, 32, photosplice_codecs::ChannelLayout::Rgb, px).unwrap();
    std::fs::write(path, photosplice_codecs::encode(&image, photosplice_codecs::Format::Png, &Default::default()).unwrap()).unwrap();
}

/// On Unix `//` followed by an absolute path names that same local file, while the resolver sees a
/// path on another computer: a place that skipped the resolver would read the file.
#[cfg(unix)]
fn network_alias(p: &Path) -> String {
    format!("/{}", p.display())
}

/// A 32×32 white document saved (as far as its links know) at `d/doc.pcraft`, with `file` placed
/// linked → (session, layer id).
fn document(d: &Path, file: &Path) -> (Session, LayerId) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 32, "height": 32})).unwrap();
    s.active_mut().unwrap().path = Some(d.join("doc.pcraft").to_string_lossy().into_owned());
    s.execute("file.placeLinked", json!({"path": file.to_string_lossy()})).unwrap();
    let id = s.active().unwrap().active_layer.unwrap();
    (s, id)
}

/// Point smart object `id` at `path` without re-rendering it.
fn relink(s: &mut Session, id: LayerId, path: &str) {
    s.edit("Relink", |doc, _| {
        if let Some(LayerContent::Smart(sm)) = doc.layer_mut(id).map(|l| &mut l.content) {
            sm.source = SmartSource::Linked { path: path.into() };
        }
        Ok(())
    })
    .unwrap();
}

fn centre(s: &Session) -> [f32; 3] {
    let p = photosplice_compose::render(&s.active().unwrap().doc, Rect::new(16, 16, 17, 17)).px[0];
    [p[0], p[1], p[2]]
}

const RED: [f32; 3] = [1.0, 0.0, 0.0];
const BLUE: [f32; 3] = [0.0, 0.0, 1.0];

fn update(s: &mut Session) -> Result<serde_json::Value, String> {
    s.execute("layer.smartObjects.updateModifiedContent", json!({})).map_err(|e| e.to_string())
}

#[test]
fn linked_smart_objects_follow_only_local_files() {
    let d = dir("links");
    png(&d.join("red.png"), [255, 0, 0]);
    png(&d.join("blue.png"), [0, 0, 255]);
    let (mut s, id) = document(&d, &d.join("red.png"));
    assert_eq!(centre(&s), RED);

    for remote in [REMOTE, "https://192.0.2.1/x.png", r"\\.\pipe\x"] {
        relink(&mut s, id, remote);
        s.start_link_scan();
        assert!(s.link_scan.is_none(), "no look at {remote}");
        assert_eq!(s.execute("layer.smartObjects.updateChanged", json!({})).unwrap(), json!({"updated": []}));
        let err = update(&mut s).unwrap_err();
        assert!(err.contains("is not opened"), "{err}");
        assert_eq!(centre(&s), RED, "the layer keeps its rendering");
    }
    let err = {
        relink(&mut s, id, REMOTE);
        update(&mut s).unwrap_err()
    };
    assert!(err.contains("is not opened: the path is on another computer"), "{err}");
    #[cfg(unix)]
    {
        relink(&mut s, id, &network_alias(&d.join("blue.png")));
        assert!(update(&mut s).is_err());
        assert_eq!(centre(&s), RED, "the file was not read");
    }

    // A relative link is taken from the document's folder.
    relink(&mut s, id, "blue.png");
    update(&mut s).unwrap();
    assert_eq!(centre(&s), BLUE);
    s.start_link_scan();
    assert!(s.link_scan.is_some(), "a followed link is looked at");
}

/// Place Linked with a path relative to the working directory stores it absolute: the stored
/// link would otherwise be taken from the document's folder.
#[cfg(unix)]
#[test]
fn place_linked_stores_an_absolute_path() {
    let d = dir("place");
    png(&d.join("red.png"), [255, 0, 0]);
    let up = std::env::current_dir().unwrap().components().count() - 1;
    let relative = std::iter::repeat_n(Path::new(".."), up).collect::<PathBuf>().join(d.join("red.png").strip_prefix("/").unwrap());
    let (s, id) = document(&d, &relative);
    let Some(LayerContent::Smart(sm)) = s.active().unwrap().doc.layer(id).map(|l| &l.content) else { panic!("not a smart object") };
    let SmartSource::Linked { path } = &sm.source else { panic!("not linked") };
    assert!(Path::new(path).is_absolute(), "{path}");
    assert_eq!(centre(&s), RED);
}

#[test]
fn package_copies_only_followed_links() {
    let d = dir("package");
    png(&d.join("red.png"), [255, 0, 0]);
    let (mut s, id) = document(&d, &d.join("red.png"));
    relink(&mut s, id, "red.png");
    #[allow(unused_mut)]
    let mut remote = vec![REMOTE.to_string()];
    #[cfg(unix)]
    remote.push(network_alias(&d.join("red.png")));
    for path in &remote {
        s.execute("file.placeLinked", json!({"path": d.join("red.png").to_string_lossy()})).unwrap();
        let other = s.active().unwrap().active_layer.unwrap();
        relink(&mut s, other, path);
    }
    let r = s.execute("file.package", json!({"dir": d.join("pkg").to_string_lossy()})).unwrap();
    let mut missing: Vec<String> = serde_json::from_value(r["missing"].clone()).unwrap();
    missing.sort();
    remote.sort();
    assert_eq!(missing, remote);
    let links = r["links"].as_array().unwrap();
    assert_eq!(links.len(), 1);
    assert!(links[0].as_str().unwrap().ends_with("red.png"), "{links:?}");
}
