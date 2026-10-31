#!/usr/bin/env python3
"""Draw the Xbox-style controller shown on the loading screen.

Writes crates/gs-vulkan/assets/xbox-pad.png, an overlay for the picture
LDCNTL in 2880 x 2160 coordinates: a soft dark patch hides the original
controller and its leader lines, and new lines lead to the same labels.
Needs Pillow.
"""
from PIL import Image, ImageChops, ImageDraw, ImageFilter

X0, Y0, W, H = 300, 500, 1290, 900  # overlay rectangle in the picture
SS = 3  # supersampling
DY = 113  # moves the pad down, so A lines up with its label


TRIGGERS = (560, 1140)
BUMPERS = (622, 1078)
TRIGGER_TOP, BUMPER_TOP = 709, 713


def sc(*v):
    """Pad coordinates to overlay texels."""
    return [(a - (X0 if i % 2 == 0 else Y0 - DY)) * SS for i, a in enumerate(v)]


def pic(x, y):
    """Picture coordinates to overlay texels."""
    return ((x - X0) * SS, (y - Y0) * SS)


def layer():
    return Image.new("L", (W * SS, H * SS), 0)


def ell(d, cx, cy, rx, ry=None, **kw):
    ry = ry or rx
    d.ellipse(sc(cx - rx, cy - ry, cx + rx, cy + ry), **kw)


def body_mask():
    m = layer()
    d = ImageDraw.Draw(m)
    d.rounded_rectangle(sc(470, 720, 1230, 1050), 150 * SS, fill=255)
    d.polygon(sc(470, 900, 395, 1130, 440, 1230, 545, 1225, 700, 1020) , fill=255)
    d.polygon(sc(1230, 900, 1305, 1130, 1260, 1230, 1155, 1225, 1000, 1020), fill=255)
    ell(d, 495, 1150, 92, 92, fill=255)
    ell(d, 1205, 1150, 92, 92, fill=255)
    d.polygon(sc(700, 1020, 1000, 1020, 930, 1080, 770, 1080), fill=255)
    return m.filter(ImageFilter.GaussianBlur(14 * SS)).point(lambda v: 255 if v > 127 else 0)


def main():
    img = Image.new("RGBA", (W * SS, H * SS), (0, 0, 0, 0))
    # Patch over the old controller, feathered into the background, and
    # strips over the old lines and the hooks by the labels.
    patch = layer()
    pd = ImageDraw.Draw(patch)
    pd.rounded_rectangle((*pic(370, 575), *pic(1450, 1385)), 120 * SS, fill=255)
    patch = patch.filter(ImageFilter.GaussianBlur(26 * SS))
    strips = layer()
    sd = ImageDraw.Draw(strips)
    for y in (626, 1038, 1278):
        sd.rectangle((*pic(1400, y - 26), *pic(1606, y + 26)), fill=255)
    strips = strips.filter(ImageFilter.GaussianBlur(4 * SS))
    patch = ImageChops.lighter(patch, strips)
    img.paste(Image.new("RGBA", img.size, (13, 3, 4, 255)), (0, 0), patch)

    # Faint red glow, like the original's.
    mask = body_mask()
    glow = mask.filter(ImageFilter.GaussianBlur(36 * SS)).point(lambda v: v // 2)
    img.paste(Image.new("RGBA", img.size, (120, 22, 18, 255)), (0, 0), glow)

    # The shoulder buttons just show over the top edge: on each side the
    # trigger, then the bumper in front of it.
    back = Image.new("RGBA", img.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(back)
    for x in TRIGGERS:
        d.rounded_rectangle(sc(x - 34, TRIGGER_TOP, x + 34, 760), 12 * SS, fill=(48, 49, 53, 255))
    for x in BUMPERS:
        d.rounded_rectangle(sc(x - 50, BUMPER_TOP, x + 50, 760), 12 * SS, fill=(74, 75, 80, 255))
    img.alpha_composite(back)

    # Body: matte black, lit from above.
    shade = Image.linear_gradient("L").resize(img.size).point(lambda v: 62 - v * 34 // 255)
    body = Image.merge("RGBA", (shade, shade, shade.point(lambda v: v + 3), mask))
    img.alpha_composite(body)
    edge = ImageChops.subtract(mask, mask.filter(ImageFilter.MinFilter(6 * SS + 1)))
    img.paste(Image.new("RGBA", img.size, (96, 97, 104, 255)), (0, 0), edge)

    d = ImageDraw.Draw(img)

    def stick(cx, cy):
        ell(d, cx, cy, 82, fill=(14, 14, 16, 255))
        ell(d, cx, cy, 68, fill=(66, 67, 72, 255))
        ell(d, cx, cy, 52, fill=(40, 41, 45, 255))

    stick(575, 860)
    stick(930, 1005)
    # D-pad.
    cx, cy = 720, 1005
    ell(d, cx, cy, 88, fill=(14, 14, 16, 255))
    d.rounded_rectangle(sc(cx - 25, cy - 70, cx + 25, cy + 70), 10 * SS, fill=(72, 73, 78, 255))
    d.rounded_rectangle(sc(cx - 70, cy - 25, cx + 70, cy + 25), 10 * SS, fill=(72, 73, 78, 255))
    # Guide, View, Menu.
    ell(d, 850, 790, 34, fill=(226, 228, 232, 255))
    ell(d, 850, 790, 24, fill=(36, 37, 40, 255))
    ell(d, 770, 875, 20, fill=(84, 85, 90, 255))
    ell(d, 930, 875, 20, fill=(84, 85, 90, 255))

    def letter(cx, cy, c, col):
        w = 7 * SS
        s = 20
        if c == "A":
            pts = [[(cx - s, cy + s), (cx, cy - s), (cx + s, cy + s)], [(cx - s / 2, cy + 5), (cx + s / 2, cy + 5)]]
        elif c == "X":
            pts = [[(cx - s, cy - s), (cx + s, cy + s)], [(cx + s, cy - s), (cx - s, cy + s)]]
        elif c == "Y":
            pts = [[(cx - s, cy - s), (cx, cy), (cx + s, cy - s)], [(cx, cy), (cx, cy + s)]]
        else:
            pts = [[(cx - 12, cy + s), (cx - 12, cy - s), (cx + 8, cy - s), (cx + 16, cy - 10), (cx + 8, cy), (cx - 12, cy), (cx + 12, cy), (cx + 20, cy + 10), (cx + 10, cy + s), (cx - 12, cy + s)]]
        for line in pts:
            d.line([tuple(sc(*p)) for p in line], fill=col, width=w, joint="curve")

    # Leader lines, under the buttons they lead to: fine and silver.
    # Each shoulder button has its own, and they merge on the way up.
    silver = (178, 180, 188, 255)
    lw = 3 * SS
    end = 1596
    trick = 626
    tops = {x: TRIGGER_TOP + DY for x in TRIGGERS} | {x: BUMPER_TOP + DY for x in BUMPERS}
    lines = [[(x, tops[x]), (x, trick)] for x in tops]
    lines.append([(min(tops), trick), (end, trick)])
    lines.append([(1085, 925 + DY), (end, 925 + DY)])
    lines.append([(1025, 862 + DY), (1025, 1278), (end, 1278)])
    for line in lines:
        d.line([pic(*p) for p in line], fill=silver, width=lw, joint="curve")
    for x, y in tops.items():
        dx, dy = pic(x, y)
        d.ellipse((dx - 4 * SS, dy - 4 * SS, dx + 4 * SS, dy + 4 * SS), fill=silver)

    r = 38
    for cx, cy, c, col in [(1145, 862, "B", (226, 62, 52)), (1025, 862, "X", (52, 134, 234)), (1085, 802, "Y", (242, 194, 46)), (1085, 925, "A", (92, 204, 64))]:
        ell(d, cx, cy, r + 4, fill=(120, 121, 126, 255))
        ell(d, cx, cy, r, fill=(24, 24, 27, 255))
        letter(cx, cy, c, col)

    img = img.resize((W, H), Image.LANCZOS)
    img.save("crates/gs-vulkan/assets/xbox-pad.png", optimize=True)


main()
