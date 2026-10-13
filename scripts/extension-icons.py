#!/usr/bin/env python3
"""Draws extension/icons/*.png from static/logo.svg.

The logo is pixel art on a 14x12 grid, so every size is drawn cell by cell at
an integer scale rather than resampled: a resampled 16 px toolbar icon is a
blue smudge. Each size picks the largest scale that leaves a margin inside
the tile, and the tile is the same dark rounded square as static/icon-192.png,
so the bird reads on a light toolbar and a dark one alike.

Run after changing the logo: python3 scripts/extension-icons.py
"""
import re
import sys
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
TILE = (15, 18, 24, 255)
BIRD = (43, 127, 212, 255)  # logo.svg's fill
# size -> (cell scale, corner radius). 128 keeps Chrome's recommended 16 px
# of transparent padding around a 96 px tile; the toolbar sizes fill theirs.
SIZES = {16: (1, 3), 32: (2, 6), 48: (3, 9), 128: (6, 18)}


def cells():
    svg = (ROOT / "static/logo.svg").read_text()
    if 'viewBox="0 0 14 12"' not in svg:
        sys.exit("logo.svg is no longer the 14x12 grid this script draws")
    rects = re.findall(r'<rect x="(\d+)" y="(\d+)" width="(\d+)" height="(\d+)"/>', svg)
    if not rects:
        sys.exit("no cells found in logo.svg")
    return [tuple(map(int, r)) for r in rects]


def draw(size, scale, radius, rects):
    im = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    pad = 16 if size == 128 else 0
    d.rounded_rectangle([pad, pad, size - 1 - pad, size - 1 - pad], radius=radius, fill=TILE)
    ox = (size - 14 * scale) // 2
    oy = (size - 12 * scale) // 2
    for x, y, w, h in rects:
        d.rectangle([ox + x * scale, oy + y * scale, ox + (x + w) * scale - 1, oy + (y + h) * scale - 1], fill=BIRD)
    return im


def main():
    out = ROOT / "extension/icons"
    out.mkdir(exist_ok=True)
    rects = cells()
    for size, (scale, radius) in SIZES.items():
        draw(size, scale, radius, rects).save(out / f"icon-{size}.png", optimize=True)
        print(f"extension/icons/icon-{size}.png")


main()
