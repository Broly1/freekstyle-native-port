#!/usr/bin/env python3
"""Build a texture pack from textures dumped with FREEK_DUMP_TEXTURES.

    tools/hd_pack.py <dump dir> [--out game/textures/hd] [--scale 4]
                     [--esrgan /path/to/realesrgan-ncnn-vulkan]

Without --esrgan the pictures are scaled with Lanczos, which is smooth but
adds no detail. With it, colour and alpha both go through Real-ESRGAN: cut-out
edges are low-resolution masks of two or three levels, and only a model turns
their stairs into clean outlines.

2D sprites (fonts, HUD, menu frames) use the line-art model and 3D surfaces the
photo model. Full-screen pictures are left out: they have their own replacements
in game/images/hd.

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
    # In a hard mask the half-covered edge texels are dark with the colour of
    # whatever the art was cut from; in soft smoke and glows they are the art.
    hard_mask = len(np.unique(alpha)) <= 4 and (alpha == 255).any()
    known = alpha == 255 if hard_mask else alpha > 0
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


def wants(path):
    w, h = Image.open(path).size
    picture = "_sprite" in path.name and "_Ct32_" in path.name
    return min(w, h) >= MIN_SIDE and not picture


def wrapped(rgb):
    rgb = np.pad(rgb, ((PAD, PAD), (PAD, PAD), (0, 0)), mode="wrap")
    return Image.fromarray(rgb.round().astype(np.uint8), "RGB")


def alpha_name(name):
    return name.removesuffix(".png") + ".alpha.png"


def prepare(path, src):
    """Save the colour of a texture, wrapped and with its alpha filled, and
    its alpha as a grey picture when it has any."""
    px = np.asarray(Image.open(path).convert("RGBA"))
    alpha = px[..., 3]
    wrapped(fill_transparent(px[..., :3], alpha)).save(src / path.name)
    if alpha.min() < 255:
        wrapped(np.repeat(alpha[..., None], 3, axis=2).astype(np.float32)).save(src / alpha_name(path.name))


def lanczos(src, dst, scale):
    for f in sorted(src.glob("*.png")):
        im = Image.open(f)
        im.resize((im.width * scale, im.height * scale), Image.LANCZOS).save(dst / f.name)


def esrgan(tool, src, dst, scale):
    groups = {"realesrgan-x4plus-anime": "_sprite", "realesrgan-x4plus": "_surface"}
    for model, kind in groups.items():
        part = src / kind
        part.mkdir()
        for f in src.glob(f"*{kind}*.png"):
            f.rename(part / f.name)
        if any(part.iterdir()):
            subprocess.run([str(tool), "-i", str(part), "-o", str(dst), "-n", model, "-s", "4", "-f", "png"], check=True, cwd=Path(tool).resolve().parent)
    if scale != 4:
        for f in sorted(dst.glob("*.png")):
            im = Image.open(f)
            im.resize((im.width * scale // 4, im.height * scale // 4), Image.LANCZOS).save(f)


def assemble(original, dst, out, scale):
    w, h = Image.open(original).size
    size = (w * scale, h * scale)
    pad = PAD * scale
    box = (pad, pad, pad + size[0], pad + size[1])
    colour = Image.open(dst / original.name).convert("RGB").crop(box)
    alpha = dst / alpha_name(original.name)
    colour.putalpha(Image.open(alpha).convert("L").crop(box) if alpha.exists() else Image.new("L", size, 255))
    colour.save(out, optimize=False, compress_level=6)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("dump", type=Path)
    p.add_argument("--out", type=Path, default=Path("game/textures/hd"))
    p.add_argument("--scale", type=int, default=4, choices=(2, 3, 4))
    p.add_argument("--esrgan", type=Path)
    a = p.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)

    have = {f.name[:16] for f in a.out.glob("*.png")}
    todo = [f for f in sorted(a.dump.glob("*.png")) if f.name[:16] not in have and wants(f)]
    print(f"{len(todo)} textures to scale, {len(have)} already in the pack")
    if not todo:
        return
    with tempfile.TemporaryDirectory(prefix="hd_pack") as tmp:
        src, dst = Path(tmp, "in"), Path(tmp, "out")
        src.mkdir()
        dst.mkdir()
        for f in todo:
            prepare(f, src)
        if a.esrgan:
            esrgan(a.esrgan, src, dst, a.scale)
        else:
            lanczos(src, dst, a.scale)
        missing = 0
        for f in todo:
            if (dst / f.name).exists():
                assemble(f, dst, a.out / f.name, a.scale)
            else:
                missing += 1
        if missing:
            sys.exit(f"the upscaler produced no output for {missing} textures")
    print(f"pack written to {a.out}")


if __name__ == "__main__":
    main()
