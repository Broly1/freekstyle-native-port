#!/usr/bin/env python3
"""Redraw the chrome menu frames and the boost gear for a texture pack.

    tools/redraw_art.py [--dump game/textures/dump] [--out game/textures/hd]
                        [--esrgan /path/to/realesrgan-ncnn-vulkan]

Their originals are too small for any upscaler to give clean edges, so only
their shapes are kept: each outline is cleaned by the line-art model
(--esrgan), smoothed at 8x and shaded from scratch as polished steel. The
mascot's face keeps its art, sharpened toward ink. Run hd_pack.py first: this replaces
its results for these textures. Needs Pillow and numpy.
"""

import argparse
import subprocess
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image

SCALE = 8
# Pieces of the chrome menu frame, in the palettes the game shows them.
FRAMES = ("2110315c89bbc353", "30bc28b9de8ef28d", "878e4a1e10974735", "a1d39d8de8a026bc", "a65dd65071a7a664", "d20c188c5378f1f0",
          "d5991324da5372a2", "dde07492c37a030f", "fef14d34934841d0")
GEAR = "04f1ae1a30c0cc38"
# Steel from shadow to highlight: position, colour.
STEEL = np.array([(0.00, 22, 25, 30), (0.28, 74, 82, 94), (0.48, 150, 160, 172),
                  (0.62, 236, 241, 246), (0.70, 255, 255, 255), (0.80, 170, 180, 192), (1.00, 96, 106, 120)], np.float32)


def box(a, r, axis):
    pad = [(0, 0), (0, 0)]
    pad[axis] = (r + 1, r)
    c = np.cumsum(np.pad(a, pad, mode="edge"), axis=axis, dtype=np.float64)
    hi = np.take(c, range(2 * r + 1, c.shape[axis]), axis=axis)
    lo = np.take(c, range(0, c.shape[axis] - 2 * r - 1), axis=axis)
    return (hi - lo) / (2 * r + 1)


def blur(a, sigma):
    """Gaussian blur, as three box blurs."""
    r = max(1, round((np.sqrt(4 * sigma * sigma + 1) - 1) / 2))
    a = a.astype(np.float64)
    for _ in range(3):
        a = box(box(a, r, 0), r, 1)
    return a.astype(np.float32)


def model(images, esrgan):
    """Run pictures through the line-art model, or nothing without one."""
    if not esrgan:
        return images
    with tempfile.TemporaryDirectory(prefix="redraw") as tmp:
        src, dst = Path(tmp, "in"), Path(tmp, "out")
        src.mkdir()
        dst.mkdir()
        for i, im in enumerate(images):
            im.convert("RGB").save(src / f"{i}.png")
        subprocess.run([str(esrgan), "-i", str(src), "-o", str(dst), "-n", "realesrgan-x4plus-anime", "-s", "4", "-f", "png"],
                       check=True, cwd=Path(esrgan).resolve().parent, capture_output=True)
        return [Image.open(dst / f"{i}.png").convert("RGB").copy() for i in range(len(images))]


def enlarge(im, size):
    return np.asarray(im.convert("L").resize(size, Image.BICUBIC), np.float32) / 255


def outline(a, sigma=SCALE * 0.5):
    """A clean edge along a mask: blurring melts what stairs are left and a
    tight threshold brings the edge back, anti-aliased."""
    return np.clip((blur(a, sigma) - 0.5) * sigma * 2 + 0.5, 0, 1)


def steel(mask):
    """Shade a mask as bevelled, polished steel, lit from the upper left."""
    height = 0.5 * blur(mask, SCALE * 0.5) + 0.3 * blur(mask, SCALE * 1.4) + 0.2 * blur(mask, SCALE * 3.5)
    dy, dx = np.gradient(height)
    k = SCALE * 3.0
    nx, ny = dx * k, dy * k
    # A broad sheen across the piece keeps flat areas from looking dead.
    h, w = mask.shape
    y, x = np.mgrid[0:h, 0:w].astype(np.float32)
    sheen = 0.09 * np.sin((x / w * 5 + y / h * 2.2) * np.pi)
    t = np.clip(0.60 + (nx + ny) * 0.75 + sheen, 0, 1)
    rgb = np.stack([np.interp(t, STEEL[:, 0], STEEL[:, c]) for c in (1, 2, 3)], axis=-1)
    # A thin dark line around the metal lifts it off any background.
    rim = np.clip(blur(mask, SCALE * 0.28) * (1 - mask) * 2.5, 0, 1)
    return rgb, rim


def source(path, esrgan):
    """Mask and colour of a texture, cleaned by the model, at 8x."""
    im = Image.open(path).convert("RGBA")
    size = (im.width * SCALE, im.height * SCALE)
    mask = im.getchannel("A").point(lambda v: 255 if v > 64 else 0)
    black = Image.new("RGB", im.size)
    black.paste(im.convert("RGB"), mask=mask)
    mask, colour = model([mask, black], esrgan)
    return enlarge(mask, size), colour.resize(size, Image.LANCZOS)


def frame(path, esrgan):
    mask, colour = source(path, esrgan)
    lum = np.asarray(colour.convert("L"), np.float32) / 255
    metal = outline(np.clip(mask * (lum > 0.24), 0, 1))
    solid = outline(mask)
    rgb, rim = steel(metal)
    inlay = np.array((9, 10, 13), np.float32)
    rgb = rgb * metal[..., None] + inlay * (1 - metal[..., None])
    a = np.maximum(np.maximum(metal, solid * 0.94), rim)
    return np.dstack([rgb, a * 255]).round().astype(np.uint8)


def inked(colour):
    """Line art sharpened toward ink, keeping the shading in the face."""
    a = np.asarray(colour, np.float32) / 255
    ink = np.clip((a.mean(axis=-1) - 0.40) * 3.2 + 0.5, 0, 1)
    ink = ink * ink * (3 - 2 * ink)
    return (a * 0.45 + ink[..., None] * 0.55) * 255


def gear(path, esrgan):
    mask, colour = source(path, esrgan)
    solid = outline(mask)
    h, w = solid.shape
    y, x = np.mgrid[0:h, 0:w].astype(np.float32)
    cx, cy = (x * solid).sum() / solid.sum(), (y * solid).sum() / solid.sum()
    r = np.hypot(x - cx, y - cy)
    rgb, _ = steel(solid)
    # The mascot sits inside the ring: fade from steel to its art there.
    keep = np.clip((0.30 * h - r) / (SCALE * 1.5), 0, 1)[..., None]
    rgb = rgb * (1 - keep) + inked(colour) * keep
    return np.dstack([rgb, solid * 255]).round().astype(np.uint8)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--dump", type=Path, default=Path("game/textures/dump"))
    p.add_argument("--out", type=Path, default=Path("game/textures/hd"))
    p.add_argument("--esrgan", type=Path)
    a = p.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)
    for f in sorted(a.dump.glob("*.png")):
        hash_ = f.name[:16]
        if hash_ in FRAMES:
            art = frame(f, a.esrgan)
        elif hash_ == GEAR:
            art = gear(f, a.esrgan)
        else:
            continue
        for old in a.out.glob(hash_ + "*.png"):
            old.unlink()
        Image.fromarray(art, "RGBA").save(a.out / f.name)
        print(f.name)


if __name__ == "__main__":
    main()
