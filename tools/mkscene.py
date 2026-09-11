#!/usr/bin/env python3
"""Build a synthetic `scene.pkg` with known geometry, for isolating transform bugs.

A real wallpaper's scene is opaque: when a layer lands in the wrong place you
cannot tell whether the model, the transform, the projection or the compositor
is at fault. This writes a scene whose every object has a position we chose, so
the expected output is arithmetic rather than opinion.

The generated scene:

* `bg` — a 3x3 colour grid that fills the design canvas exactly. Its nine
  distinct colours make orientation and coverage unambiguous.
* `marker_bl` — a 200x200 cyan quad centred at design (100, 100), i.e. the
  canvas' bottom-left corner: proves position and size.
* `group` / `child` — a quad and its parented child: proves parent composition.

`general.clearcolor` is red, so **any uncovered pixel is obviously red** — that
is how the projection-window regression (a `Fill` canvas overflowing a
mismatched output and leaving the margins bare) was caught.

Usage:
    python3 tools/mkscene.py /tmp/synth/scene.pkg
    hyprwpe set /tmp/synth/scene.pkg
"""

import json
import os
import struct
import sys

from PIL import Image, ImageDraw


def build_textures(out_dir):
    grid_path = os.path.join(out_dir, "grid.png")
    marker_path = os.path.join(out_dir, "marker.png")

    # 3x3 grid, 300x300: distinct colours so orientation is readable.
    grid = Image.new("RGB", (300, 300), (128, 128, 128))
    draw = ImageDraw.Draw(grid)
    colors = [
        (0, 0, 0), (128, 128, 128), (255, 255, 255),      # top:    black, gray, white
        (255, 0, 255), (0, 255, 0), (255, 255, 0),        # middle: magenta, green, yellow
        (0, 0, 255), (0, 255, 255), (255, 128, 0),        # bottom: blue, cyan, orange
    ]
    for r in range(3):
        for c in range(3):
            draw.rectangle(
                [c * 100, r * 100, c * 100 + 99, r * 100 + 99], fill=colors[r * 3 + c]
            )
    grid.save(grid_path)

    Image.new("RGB", (64, 64), (0, 255, 255)).save(marker_path)
    return grid_path, marker_path


def build_scene(grid_path, marker_path, out_path):
    scene = {
        "camera": {"center": "0 0 0", "eye": "0 0 1", "up": "0 1 0"},
        "general": {
            "orthogonalprojection": {"width": 3840, "height": 2160},
            "clearcolor": "1 0 0",
            "clearenabled": True,
        },
        "objects": [
            {"id": 1, "name": "bg", "origin": "1920 1080 0", "scale": "1 1 1",
             "size": "3840 2160", "image": "materials/grid.png"},
            {"id": 2, "name": "marker_bl", "origin": "100 100 0", "scale": "1 1 1",
             "size": "200 200", "image": "materials/marker.png"},
            {"id": 3, "name": "group", "origin": "960 540 0", "scale": "2 2 1",
             "size": "100 100", "image": "materials/marker.png"},
            {"id": 4, "name": "child", "parent": 3, "origin": "50 0 0",
             "scale": "1 1 1", "size": "100 100", "image": "materials/marker.png"},
        ],
        "version": 1,
    }

    files = [
        ("scene.json", json.dumps(scene, indent=2).encode()),
        ("materials/grid.png", open(grid_path, "rb").read()),
        ("materials/marker.png", open(marker_path, "rb").read()),
    ]

    # PKGV0001 layout; see crates/core/src/pkg.rs.
    version = b"PKGV0001"
    header = struct.pack("<I", len(version)) + version + struct.pack("<I", len(files))
    blob = b""
    for name, data in files:
        header += struct.pack("<I", len(name)) + name.encode()
        header += struct.pack("<II", len(blob), len(data))
        blob += data
    with open(out_path, "wb") as fh:
        fh.write(header + blob)
    return len(header) + len(blob)


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "/tmp/synth/scene.pkg"
    out_dir = os.path.dirname(out) or "."
    os.makedirs(out_dir, exist_ok=True)
    grid, marker = build_textures(out_dir)
    size = build_scene(grid, marker, out)
    print(f"wrote {out} ({size} bytes)")
    print("expected: red clear colour nowhere; grid corners TL=black TR=white "
          "BR=orange; centre=green; cyan at design (100,100) and (960,540) and (1060,540)")


if __name__ == "__main__":
    main()
