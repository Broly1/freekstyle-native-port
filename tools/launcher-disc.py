# The launcher icon: an orange flame-pattern disc titled LINUX FREEKS,
# with the game icon as its mascot. Drawn here from scratch (Pillow and
# the Noto Sans font), no game artwork.
# Usage: launcher-disc.py <out.png> <mascot.png>
import math, random, sys
from PIL import Image, ImageDraw, ImageFilter, ImageFont, ImageChops

S = 2048
OUT = sys.argv[1]
MASCOT = sys.argv[2]
FONT = "/usr/share/fonts/noto/NotoSans-BlackItalic.ttf"
FONT_B = "/usr/share/fonts/noto/NotoSans-Black.ttf"
random.seed(7)
C = S / 2
R = S * 0.49

def lerp(a, b, t): return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(len(a)))

# ---- background: radial orange -> yellow
bg = Image.new("RGB", (S, S))
px = bg.load()
small = Image.new("RGB", (256, 256))
sp = small.load()
for y in range(256):
    for x in range(256):
        d = math.hypot(x - 128 + 20, y - 128 + 40) / 150
        t = min(1, d)
        sp[x, y] = lerp((255, 214, 60), (226, 96, 8), t ** 1.3)
bg = small.resize((S, S), Image.BICUBIC)

# ---- flame tongues
def flame(cx, cy, length, width, angle, wobble, phase):
    pts_l, pts_r = [], []
    n = 28
    for i in range(n + 1):
        t = i / n
        w = width * math.sin(math.pi * min(1, t * 1.3)) ** 0.6 * (1 - t) ** 0.9
        off = wobble * math.sin(phase + t * 7.0) * t ** 1.3
        pts_l.append((off - w, -t * length))
        pts_r.append((off + w, -t * length))
    pts = pts_l + pts_r[::-1]
    ca, sa = math.cos(angle), math.sin(angle)
    return [(cx + x * ca - y * sa, cy + x * sa + y * ca) for x, y in pts]

layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(layer)
for _ in range(480):
    x, y = random.uniform(0, S), random.uniform(0, S)
    L = random.uniform(50, 160)
    a = random.uniform(-0.6, 0.6) + (math.pi if random.random() < 0.15 else 0)
    col = random.choice([(255, 236, 120, 110), (255, 245, 170, 90), (214, 80, 0, 70), (240, 140, 20, 90)])
    d.polygon(flame(x, y, L, L * 0.15, a, L * 0.45, random.uniform(0, 6)), fill=col)
big = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d2 = ImageDraw.Draw(big)
for _ in range(26):
    x, y = random.uniform(0, S), random.uniform(S * 0.2, S)
    L = random.uniform(380, 720)
    d2.polygon(flame(x, y, L, L * 0.25, random.uniform(-0.4, 0.4), L * 0.3, random.uniform(0, 6)),
               fill=random.choice([(255, 235, 110, 80), (205, 70, 0, 60)]))
big = big.filter(ImageFilter.GaussianBlur(28))
bg = bg.convert("RGBA")
bg.alpha_composite(big)
bg.alpha_composite(layer.filter(ImageFilter.GaussianBlur(1.5)))

# ---- title with tribal flame banner
TITLE = "LINUX FREEKS"
font = ImageFont.truetype(FONT, 250)
# Scale the title to the width "FREEKSTYLE" had.
_d = ImageDraw.Draw(Image.new("L", (1, 1)))
_w = sum(_d.textlength(ch, font=font) for ch in TITLE)
_ref = sum(_d.textlength(ch, font=font) for ch in "FREEKSTYLE")
font = ImageFont.truetype(FONT, int(250 * min(1.0, _ref * 1.08 / _w)))
text = Image.new("L", (S, S), 0)
td = ImageDraw.Draw(text)
widths = [td.textlength(ch, font=font) for ch in TITLE]
total = sum(widths) * 0.86
x = C - total / 2
ty = S * 0.19
for ch, w in zip(TITLE, widths):
    g = Image.new("L", (420, 420), 0)
    ImageDraw.Draw(g).text((80, 40), ch, font=font, fill=255)
    g = g.rotate(random.uniform(-9, 9), resample=Image.BICUBIC)
    dy = random.uniform(-26, 26)
    text.paste(255, (int(x - 80), int(ty - 40 + dy)), g)
    x += w * 0.86
# Spiky banner: wings on both sides plus downward drips.
banner = Image.new("L", (S, S), 0)
bd = ImageDraw.Draw(banner)
def blade(x0, y0, x1, y1, bend, width):
    pts_a, pts_b = [], []
    for i in range(21):
        t = i / 20
        bx = x0 + (x1 - x0) * t
        by = y0 + (y1 - y0) * t - bend * math.sin(math.pi * t) * (1 if x1 > x0 else 1)
        w = width * (1 - t) ** 1.2
        nx, ny = -(y1 - y0), (x1 - x0)
        nl = math.hypot(nx, ny)
        pts_a.append((bx + nx / nl * w, by + ny / nl * w))
        pts_b.append((bx - nx / nl * w, by - ny / nl * w))
    bd.polygon(pts_a + pts_b[::-1], fill=255)
top = ty + 40
for side in (-1, 1):
    ex = C + side * total * 0.47
    blade(ex, top + 160, C + side * total * 0.75, top - 170, 60 * side, 70)
    blade(ex, top + 220, C + side * total * 0.70, top + 70, 30 * side, 50)
    blade(ex - side * 120, top + 40, C + side * total * 0.58, top - 120, 40 * side, 55)
for k, dx in enumerate((-0.30, -0.08, 0.16, 0.34)):
    blade(C + total * dx, top + 220, C + total * dx + random.uniform(-40, 40), top + 270 + random.uniform(0, 60), 20, 34)
banner = ImageChops.lighter(banner, text.filter(ImageFilter.MaxFilter(41)))
outer = banner.filter(ImageFilter.MaxFilter(25))
red = Image.new("RGBA", (S, S), (205, 30, 18, 255)); red.putalpha(outer)
black = Image.new("RGBA", (S, S), (15, 8, 8, 255)); black.putalpha(banner)
shadow = Image.new("RGBA", (S, S), (60, 10, 0, 140)); shadow.putalpha(outer.filter(ImageFilter.GaussianBlur(18)).point(lambda v: v * 0.55))
bg.alpha_composite(shadow, (10, 18))
bg.alpha_composite(red)
bg.alpha_composite(black)
edge = text.filter(ImageFilter.MaxFilter(9))
dark = Image.new("RGBA", (S, S), (40, 10, 10, 255)); dark.putalpha(edge)
bg.alpha_composite(dark)
white = Image.new("RGBA", (S, S), (255, 252, 240, 255)); white.putalpha(text)
bg.alpha_composite(white)

# ---- mascot above the title
m = Image.open(MASCOT).convert("RGBA").resize((270, 270), Image.LANCZOS)
bg.alpha_composite(m, (int(C - total * 0.30 - 135), int(top - 300)))

# ---- small badge where the publisher logo was
bd = ImageDraw.Draw(bg)
bx, by = int(S * 0.76), int(S * 0.56)
bd.rounded_rectangle((bx - 170, by - 70, bx + 170, by + 70), 34, fill=(20, 12, 10, 235), outline=(255, 245, 225, 255), width=8)
f2 = ImageFont.truetype(FONT, 92)
bd.text((bx, by + 4), "FMX", font=f2, fill=(255, 196, 40, 255), anchor="mm")

# ---- player count box where the rating box was
px0, py0 = int(S * 0.20), int(S * 0.60)
bd.rounded_rectangle((px0, py0, px0 + 230, py0 + 250), 22, fill=(255, 252, 245, 245), outline=(20, 12, 10, 255), width=8)
bd.text((px0 + 115, py0 + 100), "1\u20132", font=ImageFont.truetype(FONT_B, 110), fill=(20, 12, 10, 255), anchor="mm")
bd.text((px0 + 115, py0 + 196), "PLAYERS", font=ImageFont.truetype(FONT_B, 44), fill=(20, 12, 10, 255), anchor="mm")

# ---- bottom band
band = Image.new("RGBA", (S, S), (0, 0, 0, 0))
ImageDraw.Draw(band).rectangle((0, int(S * 0.83), S, S), fill=(18, 14, 14, 255))
bg.alpha_composite(band)
f3 = ImageFont.truetype(FONT_B, 104)
bd.text((C, S * 0.895), "L I N U X  P O R T", font=f3, fill=(232, 232, 232, 255), anchor="mm")

# ---- curved rim text
f4 = ImageFont.truetype(FONT_B, 27)
rim = "FAN-MADE PORT  \u2022  NOT AFFILIATED WITH ANY PUBLISHER OR CONSOLE MAKER"
ang = math.radians(-27)
rr = R - 58
for ch in rim:
    w = f4.getlength(ch)
    g = Image.new("RGBA", (60, 60), (0, 0, 0, 0))
    ImageDraw.Draw(g).text((30, 30), ch, font=f4, fill=(70, 20, 0, 210), anchor="mm")
    a = ang + (w / 2) / rr
    g = g.rotate(-math.degrees(a) - 90, resample=Image.BICUBIC)
    gx, gy = C + rr * math.cos(a), C + rr * math.sin(a)
    if not (S * 0.80 < gy):
        bg.alpha_composite(g, (int(gx - 30), int(gy - 30)))
    ang += w / rr

# ---- gloss
gloss = Image.new("L", (S, S), 0)
ImageDraw.Draw(gloss).ellipse((-S * 0.3, -S * 0.75, S * 1.1, S * 0.45), fill=46)
gloss = gloss.filter(ImageFilter.GaussianBlur(60))
gw = Image.new("RGBA", (S, S), (255, 255, 255, 255)); gw.putalpha(gloss)
bg.alpha_composite(gw)

# ---- disc shape: rim, hub, hole
mask = Image.new("L", (S, S), 0)
md = ImageDraw.Draw(mask)
md.ellipse((C - R, C - R, C + R, C + R), fill=255)
hole = S * 0.065
md.ellipse((C - hole, C - hole, C + hole, C + hole), fill=0)
disc = Image.new("RGBA", (S, S), (0, 0, 0, 0))
disc.paste(bg, (0, 0), mask)
dd = ImageDraw.Draw(disc)
dd.ellipse((C - R, C - R, C + R, C + R), outline=(200, 200, 205, 255), width=14)
hub = S * 0.11
hubl = Image.new("RGBA", (S, S), (0, 0, 0, 0))
hd = ImageDraw.Draw(hubl)
hd.ellipse((C - hub, C - hub, C + hub, C + hub), fill=(25, 18, 16, 255))
hd.ellipse((C - hub * 0.86, C - hub * 0.86, C + hub * 0.86, C + hub * 0.86), outline=(110, 100, 95, 255), width=6)
hd.ellipse((C - hole - 8, C - hole - 8, C + hole + 8, C + hole + 8), outline=(170, 170, 175, 255), width=8)
hm = Image.new("L", (S, S), 0)
ImageDraw.Draw(hm).ellipse((C - hole, C - hole, C + hole, C + hole), fill=255)
hubl.putalpha(ImageChops.subtract(hubl.getchannel("A"), hm))
disc.alpha_composite(hubl)

disc.resize((1024, 1024), Image.LANCZOS).save(OUT)
print("saved", OUT)
