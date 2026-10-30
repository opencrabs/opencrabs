#!/usr/bin/env python3
"""Regenerate src/assets/icon.icns from the brand app icon.

macOS reads an app icon from a bundle's .icns container. The container holds a
ladder of sizes so Finder, the Dock and Launchpad can each pick the one that
matches the display scale. A ladder that stops at 256 is upscaled on every
Retina display, which is what this script fixes.

The source is src/assets/icon.png, the same artwork build.rs embeds in the
Windows executable, so both platforms show the same icon. It is full bleed:
no padding, no transparent canvas, nothing cropped or stretched.

The only designed master in the tree is 256x256, so the 512 and 1024 members
are upscaled. They are still correct (the container is what macOS reads) but a
native 1024 master would render sharper; drop one in and pass --source.

Usage:
    python3 scripts/gen-iconset.py               # writes src/assets/icon.icns
    python3 scripts/gen-iconset.py --out DIR     # writes an .iconset directory
    python3 scripts/gen-iconset.py --source X.png
"""

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw

REPO = Path(__file__).resolve().parent.parent
SOURCE = REPO / "src" / "assets" / "icon.png"
TARGET = REPO / "src" / "assets" / "icon.icns"

# macOS app icons are not square artwork. Every app on the system draws the
# same tile: a rounded square, inset from the canvas edge, sitting at a
# consistent visual weight in the Dock. The proportions below are Apple's
# 1024px template (100px margin per side, 185px corner radius) expressed as
# fractions so they scale with the canvas.
TILE_INSET = 100 / 1024
TILE_RADIUS = 185 / 1024

# Shape once at twice the largest member, then downsample. Drawing the corners
# at 2048 and halving them keeps the curve clean at every size in the ladder.
SHAPE_SUPERSAMPLE = 2

# (filename, pixel size). The @2x entries are the retina variants of the
# smaller logical sizes; together they are the ten members of a full ladder.
LADDER = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
]


def macos_tile(master: Image.Image) -> Image.Image:
    """Draw the artwork as a macOS app tile: rounded square, inset on a
    transparent canvas. Clipping to the tile is what makes Finder and the Dock
    show a normal app icon instead of a full-bleed rectangle."""
    canvas = LADDER[-1][1] * SHAPE_SUPERSAMPLE
    inset = round(canvas * TILE_INSET)
    radius = round(canvas * TILE_RADIUS)
    body = canvas - inset * 2

    tile = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    art = master.resize((body, body), Image.LANCZOS)

    mask = Image.new("L", (body, body), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, body - 1, body - 1), radius=radius, fill=255
    )

    tile.paste(art, (inset, inset), mask)
    return tile


def build(source: Path, iconset: Path, round_corners: bool = True) -> None:
    if not source.is_file():
        sys.exit(f"source icon is missing: {source}")

    master = Image.open(source).convert("RGBA")
    side = min(master.size)
    if master.size[0] != master.size[1]:
        # Keep the icon square by trimming the longer edge, centred. The shipped
        # master is already square, so this only matters for a replacement.
        left = (master.size[0] - side) // 2
        top = (master.size[1] - side) // 2
        master = master.crop((left, top, left + side, top + side))

    shaped = macos_tile(master) if round_corners else master

    iconset.mkdir(parents=True, exist_ok=True)
    for name, size in LADDER:
        # Resizing every member from the shaped master (not chaining up from the
        # previous one) keeps the small sizes clean.
        shaped.resize((size, size), Image.LANCZOS).save(iconset / name)

    print(f"wrote {len(LADDER)} members to {iconset}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=SOURCE)
    parser.add_argument("--out", type=Path, default=None)
    args = parser.parse_args()

    if args.out is not None:
        build(args.source, args.out)
        return

    with tempfile.TemporaryDirectory() as tmp:
        iconset = Path(tmp) / "icon.iconset"
        build(args.source, iconset)
        subprocess.run(
            ["iconutil", "-c", "icns", str(iconset), "-o", str(TARGET)],
            check=True,
        )
    print(f"wrote {TARGET} ({TARGET.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
