#!/usr/bin/env python3
"""Build a texture pack from textures dumped with FREEK_DUMP_TEXTURES.

    tools/hd_pack.py <dump dir> [--out game/textures/hd] [--scale 4]
                     [--esrgan /path/to/realesrgan-ncnn-vulkan]

Without --esrgan the pictures are scaled with Lanczos, which is smooth but
adds no detail. With it, colour goes through Real-ESRGAN. Alpha is always
scaled with Lanczos: a model would invent detail in cut-out edges and font
outlines.

Textures first used as 2D sprites (fonts, menus, HUD) are left out unless
--sprites is given: the game sharpens those itself with xBRZ, and a model
blurs and greys small text.

Files already in the pack are left alone, so a new dump only adds what is
missing and hand-made replacements survive. Needs Pillow and numpy.
"""

import argparse
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image

# Effect ramps and lookup tables: their exact texels matter.
MIN_SIDE = 16
# Wrapped border around a texture while it is scaled, so tiling surfaces
# keep matching at their edges.
PAD = 16


def fill_transparent(rgb, alpha):
    """Spread colour into see-through texels so that scaling does not drag
    whatever colour they hold into the edges of cut-outs."""
    rgb = rgb.astype(np.float32).copy()
    known = alpha > 0
    if known.all() or not known.any():
        return rgb
    for _ in range(max(rgb.shape[:2])):
        if known.all():
            break
        total, count = np.zeros_like(rgb), np.zeros(known.shape, np.float32)
        for axis, step in ((0, 1), (0, -1), (1, 1), (1, -1)):
            total += np.roll(rgb * known[..., None], step, axis)
            count += np.roll(known, step, axis)
        grow = ~known & (count > 0)
        rgb[grow] = total[grow] / count[grow, None]
        known = known | grow
    return rgb


def wants(path, sprites):
    w, h = Image.open(path).size
    return min(w, h) >= MIN_SIDE and (sprites or "_sprite" not in path.name)


def prepare(path, dest):
    """Save the colour of a texture, wrapped and with its alpha filled."""
    px = np.asarray(Image.open(path).convert("RGBA"))
    rgb = fill_transparent(px[..., :3], px[..., 3])
    rgb = np.pad(rgb, ((PAD, PAD), (PAD, PAD), (0, 0)), mode="wrap")
    Image.fromarray(rgb.round().astype(np.uint8), "RGB").save(dest)


def lanczos(src, dst, scale):
    for f in sorted(src.glob("*.png")):
        im = Image.open(f)
        im.resize((im.width * scale, im.height * scale), Image.LANCZOS).save(dst / f.name)


def esrgan(tool, src, dst, scale):
    model = "realesrgan-x4plus"
    subprocess.run([str(tool), "-i", str(src), "-o", str(dst), "-n", model, "-s", "4", "-f", "png"], check=True, cwd=Path(tool).resolve().parent)
    if scale != 4:
        for f in sorted(dst.glob("*.png")):
            im = Image.open(f)
            im.resize((im.width * scale // 4, im.height * scale // 4), Image.LANCZOS).save(f)


def assemble(original, scaled, out, scale):
    px = Image.open(original).convert("RGBA")
    size = (px.width * scale, px.height * scale)
    pad = PAD * scale
    colour = Image.open(scaled).convert("RGB").crop((pad, pad, pad + size[0], pad + size[1]))
    alpha = px.getchannel("A")
    alpha = Image.new("L", size, 255) if alpha.getextrema() == (255, 255) else alpha.resize(size, Image.LANCZOS)
    colour.putalpha(alpha)
    colour.save(out, optimize=False, compress_level=6)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("dump", type=Path)
    p.add_argument("--out", type=Path, default=Path("game/textures/hd"))
    p.add_argument("--scale", type=int, default=4, choices=(2, 3, 4))
    p.add_argument("--esrgan", type=Path)
    p.add_argument("--sprites", action="store_true", help="also scale 2D sprites")
    a = p.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)

    have = {f.name[:16] for f in a.out.glob("*.png")}
    todo = [f for f in sorted(a.dump.glob("*.png")) if f.name[:16] not in have and wants(f, a.sprites)]
    print(f"{len(todo)} textures to scale, {len(have)} already in the pack")
    if not todo:
        return
    with tempfile.TemporaryDirectory(prefix="hd_pack") as tmp:
        src, dst = Path(tmp, "in"), Path(tmp, "out")
        src.mkdir()
        dst.mkdir()
        for f in todo:
            prepare(f, src / f.name)
        if a.esrgan:
            esrgan(a.esrgan, src, dst, a.scale)
        else:
            lanczos(src, dst, a.scale)
        missing = 0
        for f in todo:
            if (dst / f.name).exists():
                assemble(f, dst / f.name, a.out / f.name, a.scale)
            else:
                missing += 1
        if missing:
            sys.exit(f"the upscaler produced no output for {missing} textures")
    print(f"pack written to {a.out}")


if __name__ == "__main__":
    main()
