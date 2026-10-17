#!/usr/bin/env python3
"""Write background.svg from background.src.svg: its text outlined in Roboto (see README.md here).

Needs usvg on PATH, fontTools (`pip install fonttools`) and Roboto's variable font from Google
Fonts (OFL 1.1, `OFL-Roboto.txt`; https://github.com/google/fonts/tree/main/ofl/roboto):

    python3 packaging/macos/dmg/outline.py path/to/Roboto[wdth,wght].ttf

The font itself is not kept in the repository: only the outlines of the few words the artwork
uses end up in background.svg.
"""

import os
import re
import subprocess
import sys
import tempfile

from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

HERE = os.path.dirname(os.path.abspath(__file__))
ICON = "../../../assets/app-icon/photosplice-small.svg"
# The weights background.src.svg uses (usvg matches font-weight in whole hundreds).
WEIGHTS = (400, 600)


def static_weights(variable, out_dir):
    """Static instances of the variable font, one per weight, all in the family Roboto."""
    for w in WEIGHTS:
        font = instantiateVariableFont(TTFont(variable), {"wght": w, "wdth": 100})
        font["OS/2"].usWeightClass = w
        for record in font["name"].names:
            if record.nameID in (1, 16):
                record.string = "Roboto"
            elif record.nameID in (2, 17):
                record.string = f"W{w}"
            elif record.nameID == 4:
                record.string = f"Roboto W{w}"
            elif record.nameID == 6:
                record.string = f"Roboto-W{w}"
        font.save(os.path.join(out_dir, f"Roboto-W{w}.ttf"))


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    with tempfile.TemporaryDirectory() as tmp:
        static_weights(sys.argv[1], tmp)
        out = os.path.join(tmp, "outlined.svg")
        subprocess.run(
            ["usvg", "--skip-system-fonts", "--use-fonts-dir", tmp, "--resources-dir", HERE, os.path.join(HERE, "background.src.svg"), out],
            check=True,
        )
        with open(out, encoding="utf-8") as f:
            svg = f.read()
    # usvg embeds the linked icon: link the file again, so the artwork follows the icon.
    svg, n = re.subn(r'xlink:href="data:image/svg\+xml;base64,[^"]*"', f'xlink:href="{ICON}"', svg)
    if n != 1:
        sys.exit(f"error: expected one embedded icon, found {n}")
    with open(os.path.join(HERE, "background.svg"), "w", encoding="utf-8") as f:
        f.write(svg)


if __name__ == "__main__":
    main()
