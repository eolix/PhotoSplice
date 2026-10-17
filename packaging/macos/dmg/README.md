# macOS DMG window

What Finder shows when the DMG opens: a 660 × 400 pt background with the app icon and the
`Applications` link side by side. `package.sh` copies these files into the image; nothing here is
generated at build time, so the DMG still builds with `hdiutil makehybrid` (no mounted device, no
Finder scripting on CI, nothing extra installed in the signing job).

| File | What |
|---|---|
| `background.src.svg` | The artwork as edited: the app icon tile (`assets/app-icon/photosplice-small.svg`, linked, not copied) on a navy panel (the icon's own colours) beside a light field with the PhotoSplice wordmark, a rule in the icon's cyan-to-orange gradient, the arrow and the instruction, in live Roboto text (Google Fonts, OFL 1.1: `OFL-Roboto.txt`). |
| `background.svg` | `background.src.svg` with its text outlined by `outline.py`, so rendering it needs no fonts. |
| `background.tiff` | The background at 1x (660 × 400 px, 72 dpi) and 2x (1320 × 800 px, 144 dpi) in one HiDPI TIFF (RGB, Deflate, sRGB; about 100 KB: the icon's gradients band in a palette). Goes to `.background/background.tiff`. |
| `dmg-layout.DS_Store` | Finder's view settings for the volume: window size, icon size 128, PhotoSplice.app at (326, 205), `Applications` at (574, 205), and the background. Goes to `.DS_Store` in the image; named so it isn't mistaken for (or ignored like) a Finder-generated `.DS_Store`. |
| `outline.py` | Writes `background.svg` from `background.src.svg`. |
| `generate.py` | Writes `background.tiff` and `dmg-layout.DS_Store` from `background.svg` and the layout above. |
| `OFL-Roboto.txt` | Licence of Roboto, whose outlines the artwork carries. |

## The volume name has no version

The mounted volume is called `PhotoSplice`, not `PhotoSplice <version>`. The layout points at the background
through an alias that includes the volume name, and Finder resolves it by that name: with a
versioned name the window keeps its size and icon positions but shows no background (tested).
The DMG file name (`photosplice-<version>-macos-<arch>.dmg`) still carries the version.

## Rules

- **Finder draws the icon labels in black in light and dark mode** when a window has a background,
  so the area under both icons stays light (Paper).
- **Nothing goes inside the icon boxes:** artwork keeps 10 pt clear of each 128 pt icon box and of
  the label strip under it.

## Regenerate

Edit `background.src.svg` (or the layout constants in `generate.py`), outline its text, then
write the TIFF and the layout, on any OS:

```sh
cargo install resvg usvg
pip install 'pillow>=12' ds_store==1.3.3 mac_alias==2.2.3 fonttools
python3 packaging/macos/dmg/outline.py path/to/Roboto[wdth,wght].ttf   # writes background.svg
python3 packaging/macos/dmg/generate.py                                # needs resvg on PATH
```

`outline.py` takes Roboto's variable font from Google Fonts
(https://github.com/google/fonts/tree/main/ofl/roboto), cuts the weights the artwork uses and
outlines the text with them; the font is not kept here. Its licence is `OFL-Roboto.txt`.

Both outputs are byte-for-byte reproducible and carry nothing from the machine that ran it: the
outlined SVG renders without any fonts, and `dmg-layout.DS_Store` is written from scratch, its background
alias holding only the volume name and `/.background/background.tiff`. The window is 660 × 432 with Finder's 32 pt
title bar; the content area is 660 × 400.

This follows VectorCraft's `packaging/macos/dmg/` (storytold/vectorcraft#493).
