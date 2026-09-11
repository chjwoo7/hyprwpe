#!/usr/bin/env python3
"""Build a synthetic `scene.pkg` whose object is *animated*, to prove keyframe
sampling.

The marker starts at design (400, 400), travels to (3400, 1800) and back over
120 frames at 30 fps in `loop` mode, so it is always somewhere predictable. Two
checks are possible:

* offline: render at fixed times and compare against the expected positions
  ```
  cargo run -q --release -p hyprwpe-render --example scenecompose -- \
      /tmp/anim2/scene.pkg /tmp/anim2/t1.png 3840 2160 fill 1
  # t=0 -> (400,400)   t=1 -> (1900,1100)   t=2 -> (3400,1800)
  ```
* live: `hyprwpe set /tmp/anim2/scene.pkg` and take screenshots a fraction of a
  second apart — the cyan centroid must move.

Usage: python3 tools/mkanim.py [/tmp/anim2/scene.pkg]
"""

import json
import os
import struct
import sys

from PIL import Image


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "/tmp/anim2/scene.pkg"
    out_dir = os.path.dirname(out) or "."
    os.makedirs(out_dir, exist_ok=True)

    bg = os.path.join(out_dir, "bg.png")
    marker = os.path.join(out_dir, "marker.png")
    Image.new("RGB", (256, 256), (40, 40, 40)).save(bg)
    Image.new("RGB", (128, 128), (0, 255, 255)).save(marker)

    scene = {
        "general": {
            "orthogonalprojection": {"width": 3840, "height": 2160},
            "clearcolor": "1 0 0",
            "clearenabled": True,
        },
        "objects": [
            {"id": 1, "name": "bg", "origin": "1920 1080 0", "scale": "1 1 1",
             "size": "3840 2160", "image": "materials/bg.png"},
            {
                "id": 2, "name": "marker",
                # An animated property is `{ value, animation }`: the base value
                # and the keyframes under one key.
                "origin": {"value": "400 400 0", "animation": {
                    "c0": [{"frame": 0, "value": 400},
                           {"frame": 60, "value": 3400},
                           {"frame": 120, "value": 400}],
                    "c1": [{"frame": 0, "value": 400},
                           {"frame": 60, "value": 1800},
                           {"frame": 120, "value": 400}],
                    "options": {"fps": 30, "length": 120, "mode": "loop"},
                }},
                "scale": "1 1 1", "size": "128 128",
                "image": "materials/marker.png",
            },
        ],
        "version": 1,
    }

    files = [
        ("scene.json", json.dumps(scene, indent=2).encode()),
        ("materials/bg.png", open(bg, "rb").read()),
        ("materials/marker.png", open(marker, "rb").read()),
    ]
    version = b"PKGV0001"
    header = struct.pack("<I", len(version)) + version + struct.pack("<I", len(files))
    blob = b""
    for name, data in files:
        header += struct.pack("<I", len(name)) + name.encode()
        header += struct.pack("<II", len(blob), len(data))
        blob += data
    with open(out, "wb") as fh:
        fh.write(header + blob)
    print(f"wrote {out}")
    print("expect marker centre: t=0 (400,400), t=1 (1900,1100), t=2 (3400,1800)")


if __name__ == "__main__":
    main()
