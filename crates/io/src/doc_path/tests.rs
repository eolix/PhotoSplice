use std::path::Path;

use super::*;

/// What `raw` is, without a document: `None` when it is a local path.
fn kind(raw: &str) -> Option<Refused> {
    lexical(raw, None).err()
}

fn check(want: Option<Refused>, cases: &[&str]) {
    for raw in cases {
        assert_eq!(kind(raw), want, "{raw:?}");
    }
}

fn slashed(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[test]
fn relative_paths_are_taken_from_the_document_folder() {
    let doc = std::env::temp_dir().join("art").join("poster.pcraft");
    assert_eq!(resolve("logo.png", Some(&doc)), Ok(std::env::temp_dir().join("art").join("logo.png")));
    assert_eq!(resolve("logo.png", None), Err(NotFollowed::NoDocumentFolder));
    assert_eq!(resolve(r"\\192.0.2.1\share\x.png", Some(&doc)), Err(NotFollowed::Refused(Refused::Network)));
    assert_eq!(resolve("//192.0.2.1/share/x.png", None), Err(NotFollowed::Refused(Refused::Network)));
}

#[test]
fn local_paths_are_followed() {
    check(
        None,
        &[
            r"C:\x",
            "C:/x",
            r"\\?\C:\x",
            r"\\?\C:\",
            r"\\?\D:\long\folder\file.txt.",
            "/home/u/x",
            "/",
            "img/a.png",
            r"img\a.png",
            "./a.png",
            "../shared/a.png",
            "C:x",
            r"\x",
            "console.txt",
            "nul_report.txt",
            "com10.txt",
            "auxiliary/a.png",
            "file:///C:/x/a.png",
            "file:///home/u/a.png",
            "file://localhost/C:/x",
            "FILE://LOCALHOST/home/u/x",
            "file:/C|/x",
            "file:img/a.png",
            "file://C:/x",
            r"file:\\C:\x",
            "file:///C:/caf%E9.png",
            r"C:\\host\share",
            r"./\\host\share\x",
            "file:///D:",
        ],
    );
}

#[test]
fn paths_to_other_computers_are_refused() {
    check(
        Some(Refused::Network),
        &[
            r"\\h\s",
            r"\\h\s\x.png",
            "//h/s/x.png",
            r"\/h\s",
            r"/\h\s",
            r"\\h",
            r"\\",
            r"\\\h\s",
            r"\\?\UNC\h\s\x",
            r"\\?\unc\h\s",
            "//?/UNC/h/s/x",
            r"\\.\UNC\h\s\x",
            r"\\h@SSL@443\DavWWWRoot\x",
            "file://h/s/x.png",
            "file:////h/s/x.png",
            r"file:\\h\s\x.png",
            "file:%5C%5Ch%5Cs%5Cx.png",
            "file:///%5C%5Ch/s/x.png",
            "file:%2F%2Fh/s/x",
        ],
    );
}

#[test]
fn device_paths_are_refused() {
    check(
        Some(Refused::Device),
        &[
            r"\\.\C:",
            r"\\.\PhysicalDrive0",
            r"\\.\pipe\x",
            "//./pipe/x",
            r"\\.",
            r"\??\C:\x",
            "/??/C:/x",
            r"\\?",
            r"\\?\",
            r"\\?\C:",
            r"\\?\C:x",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\x",
            "//?/C:/x",
            r"/\?\C:\x",
            r"\\?/C:\x",
            "CON",
            "nul.tar.gz",
            "PRN...",
            "aux .png",
            "COM1",
            "COM¹",
            "lpt9.txt",
            "conout$",
            "NUL:",
            r"C:\docs\nul",
            "img/CON/a.png",
            "file:///C:/nul",
        ],
    );
}

#[test]
fn urls_are_refused() {
    check(
        Some(Refused::Url),
        &[
            "http://h/x.png",
            "HTTPS://h/x.png",
            "ms-word:ofe|u|https://h/x.docx",
            "smb://h/s/x",
            "mailto:a@b",
            "svn+ssh://h/x",
            "file:http://h/x",
            r"http:\\h\x",
            "data:image/png;base64,AAAA",
            "javascript:alert(1)",
            "shell:startup",
            "jar:file:///C:/x.jar!/a.png",
            "view-source:x",
            "Data:2024/a.png",
            "git+https:h/x",
            "x-app:https://h/x",
            "http:/h/x",
            "ab:",
            "ab:/x",
        ],
    );
}

#[test]
fn colons_in_names_are_refused_only_on_windows() {
    let scheme_like = ["chapter1:intro.png", "ab:c", "Shoot: Day 1/a.jpg", "notes:v2.pdf"];
    let in_name = ["img/fig:1.png", r"C:\x:stream", "/home/u/a:b", "2024:05/a.jpg", "/Users/a/2024:05/x.jpg", "file:///C:/a%3Ab"];
    if cfg!(windows) {
        check(Some(Refused::Url), &scheme_like);
        check(Some(Refused::Invalid), &in_name);
    } else {
        check(None, &scheme_like);
        check(None, &in_name);
    }
}

#[test]
fn unusable_paths_are_refused() {
    check(
        Some(Refused::Invalid),
        &[
            "",
            "a\0b",
            "a\nb",
            "a\u{85}b",
            "file:",
            "file://",
            "file://localhost",
            "file:#x",
            "file:///C:/a%00b",
            "file:///C:/%81",
            r"C:\..\x",
            r"C:\a\..\..\x",
            "/..",
            r"\\?\C:\a\..\b",
            r"\\?\C:\a/b",
            r"\\?\C:\a\\b",
            r"C:\a\.. \b",
            r"C:\a\...\b",
            "a\u{FF3C}b",
            "a\u{2215}b",
            "file:///C:/a%EF%BC%BCb",
        ],
    );
}

#[test]
fn links_resolve_against_the_document() {
    let doc = Path::new(r"C:\docs\report\a.doc");
    let ok = |raw: &str| slashed(&lexical(raw, Some(doc)).unwrap());
    assert_eq!(lexical(r"img\x.png", Some(doc)).unwrap(), Path::new(r"C:\docs\report\img\x.png"));
    assert_eq!(ok("../shared/x.png"), "C:/docs/shared/x.png");
    assert_eq!(ok(r"\x.png"), "C:/x.png");
    assert_eq!(ok("C:x.png"), "C:/docs/report/x.png");
    assert_eq!(ok("D:x.png"), "D:x.png");
    assert_eq!(ok(r"C:..\..\x.png"), "C:/x.png");
    assert_eq!(lexical(r"..\..\..\x.png", Some(doc)), Err(Refused::Invalid));
    assert_eq!(ok("file:///D:/My%20Pictures/a%23b.png#frag"), "D:/My Pictures/a#b.png");
    assert_eq!(ok("file:///C:/caf%E9/na%C3%AFve%80.png"), "C:/café/naïve€.png");

    let unix = Path::new("/home/u/docs/a.doc");
    assert_eq!(slashed(&lexical(r"img\x.png", Some(unix)).unwrap()), "/home/u/docs/img/x.png");
    assert_eq!(slashed(&lexical("../x.png", Some(unix)).unwrap()), "/home/u/x.png");
    // The document's own name may contain colons.
    assert_eq!(slashed(&lexical("x.png", Some(Path::new("/home/u/a:b/c:d.doc"))).unwrap()), "/home/u/a:b/x.png");

    let verbatim = Path::new(r"\\?\C:\docs\a.doc");
    assert_eq!(lexical("img/../x.png", Some(verbatim)).unwrap(), Path::new(r"\\?\C:\docs\x.png"));
    // A document on a device path lends no folder.
    assert_eq!(lexical("x.png", Some(Path::new(r"\\?\GLOBALROOT\Device\X\a.doc"))), Err(Refused::Device));
}

#[test]
fn only_the_documents_own_share_is_followed() {
    let doc = Path::new(r"\\Server\Share\docs\a.doc");
    assert_eq!(lexical(r"img\x.png", Some(doc)).unwrap(), Path::new(r"\\Server\Share\docs\img\x.png"));
    assert_eq!(lexical(r"\x.png", Some(doc)).unwrap(), Path::new(r"\\Server\Share\x.png"));
    assert!(lexical("//SERVER/SHARE/img/x.png", Some(doc)).is_ok());
    assert!(lexical(r"\\?\UNC\server\share\x.png", Some(doc)).is_ok());
    assert!(lexical("file://localhost//server/share/x.png", Some(doc)).is_ok());

    assert_eq!(lexical(r"\\other\share\x.png", Some(doc)), Err(Refused::Network));
    assert_eq!(lexical(r"\\server\other\x.png", Some(doc)), Err(Refused::Network));
    assert_eq!(lexical(r"\\server\share\..\other\x.png", Some(doc)), Err(Refused::Invalid));
    assert_eq!(lexical("file://server/share/x.png", Some(doc)), Err(Refused::Network));
    assert_eq!(lexical(r"\\server\share\nul", Some(doc)), Err(Refused::Device));
    assert_eq!(lexical(r"\\server\share\x.png", Some(Path::new(r"C:\docs\a.doc"))), Err(Refused::Network));
}

#[test]
fn followed_paths_are_absolute() {
    for raw in ["img/x.png", "../x.png", "file:img/x.png"] {
        assert_eq!(resolve(raw, None), Err(NotFollowed::NoDocumentFolder), "{raw:?}");
    }
    assert_eq!(resolve("https://h/x.png", None), Err(NotFollowed::Refused(Refused::Url)));
    if cfg!(unix) {
        assert_eq!(resolve("/x.png", None), Ok("/x.png".into()));
        assert_eq!(resolve("file:///home/u/x.png", None), Ok("/home/u/x.png".into()));
        // Windows forms are not absolute paths here.
        assert_eq!(resolve(r"C:\x.png", None), Err(NotFollowed::NoDocumentFolder));
        // A drive-relative link that the document's drive does not resolve.
        assert_eq!(resolve("D:x.png", Some(Path::new("/home/u/a.doc"))), Err(NotFollowed::Refused(Refused::Invalid)));
    }
    for why in [NotFollowed::NoDocumentFolder, NotFollowed::Refused(Refused::Network)] {
        assert!(!why.to_string().is_empty());
    }
}
