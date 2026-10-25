#!/usr/bin/env python3
"""Make 16:9 versions of the 4:3 high-resolution pictures for --widescreen.

    tools/widen-pictures.py [game/images/hd] [game/images/hd/wide]

Each picture is stretched to 3840x2160 with ffmpeg, the edges more than
the centre: a point at v (-1..1 across the 4:3 picture) lands at u across
the wide one, where v = A*u + (1 - A)*u^3. The renderer moves the game's
overlays on a picture by the same curve (WIDE_CENTRE in
crates/gs-vulkan/src/pictures.rs), so A must match it.

Needs ffmpeg and numpy. The remap filter samples the nearest pixel, so it
works at twice the size and scales down after.
"""

import os
import subprocess
import sys
import tempfile

import numpy as np

A = 1.2
OUT_W, OUT_H = 3840, 2160
# Supersampled source (4:3) and output sizes.
SW, SH, OW, OH = 2 * OUT_H * 4 // 3, 2 * OUT_H, 2 * OUT_W, 2 * OUT_H


def pgm(path, arr):
    with open(path, "wb") as f:
        f.write(b"P5\n%d %d\n65535\n" % (arr.shape[1], arr.shape[0]))
        f.write(arr.astype(">u2").tobytes())


def main():
    src = sys.argv[1] if len(sys.argv) > 1 else "game/images/hd"
    dst = sys.argv[2] if len(sys.argv) > 2 else os.path.join(src, "wide")
    os.makedirs(dst, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        u = (np.arange(OW) + 0.5) / OW * 2 - 1
        v = A * u + (1 - A) * u**3
        sx = np.clip(np.round((v + 1) / 2 * SW - 0.5), 0, SW - 1)
        xmap, ymap = os.path.join(tmp, "x.pgm"), os.path.join(tmp, "y.pgm")
        pgm(xmap, np.tile(sx, (OH, 1)))
        pgm(ymap, np.tile(np.arange(OH)[:, None], (1, OW)))
        names = sorted(n for n in os.listdir(src) if n.lower().endswith(".png"))
        for name in names:
            graph = (
                f"[0]scale={SW}:{SH}:flags=lanczos,format=rgb24[s];"
                f"[s][1][2]remap,scale={OUT_W}:{OUT_H}:flags=lanczos"
            )
            subprocess.run(
                ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
                 "-i", os.path.join(src, name), "-i", xmap, "-i", ymap,
                 "-filter_complex", graph, os.path.join(dst, name)],
                check=True,
            )
            print(name)


if __name__ == "__main__":
    main()
