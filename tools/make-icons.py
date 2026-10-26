# Make the game and launcher pictures without any game artwork: the game
# icon is a grinning badge drawn here, the launcher icon is cut from
# docs/icons/launcher-disc-source.png, and the launcher title banner is
# drawn here too. Writes SVGs to docs/icons/ and PNGs at every hicolor
# size to freekstyle/assets/{icon,launcher}/ (needs rsvg-convert and
# ImageMagick).
import math, pathlib, subprocess

ROOT = pathlib.Path(__file__).resolve().parent.parent
SIZES = [16, 22, 24, 32, 48, 64, 128, 256, 512]

# Palette: rim red, deep maroon, off-white (as the launcher theme).
RED, RED_DARK, MAROON, WHITE = "#ec2a1f", "#9d0c0c", "#4f0710", "#fff6ee"


def badge(cx=256, cy=270, s=1.0):
    """The grinning badge: a red ring, maroon field, devilish face."""
    t = f'translate({cx} {cy}) scale({s}) translate(-256 -270)'
    return f'''
  <g transform="{t}">
    <circle cx="256" cy="270" r="214" fill="url(#rim)" stroke="{MAROON}" stroke-width="12"/>
    <circle cx="256" cy="270" r="184" fill="{WHITE}"/>
    <circle cx="256" cy="270" r="166" fill="url(#field)"/>
    <!-- face -->
    <path d="M256 140 C340 140 382 200 382 268 C382 346 324 398 256 398 C188 398 130 346 130 268 C130 200 172 140 256 140 Z" fill="{WHITE}"/>
    <!-- brows and slanted eyes -->
    <path d="M154 214 L238 252 L232 268 L150 234 Z" fill="{MAROON}"/>
    <path d="M358 214 L274 252 L280 268 L362 234 Z" fill="{MAROON}"/>
    <path d="M168 250 C190 246 222 262 236 280 C212 296 182 292 166 270 Z" fill="{RED_DARK}"/>
    <path d="M344 250 C322 246 290 262 276 280 C300 296 330 292 346 270 Z" fill="{RED_DARK}"/>
    <circle cx="214" cy="274" r="9" fill="{WHITE}"/>
    <circle cx="298" cy="274" r="9" fill="{WHITE}"/>
    <!-- grin with zigzag teeth -->
    <path d="M168 312 C210 342 302 342 344 312 C334 364 298 386 256 386 C214 386 178 364 168 312 Z" fill="{MAROON}"/>
    <path d="M178 320 L196 346 L212 330 L230 352 L246 334 L256 356 L266 334 L282 352 L300 330 L316 346 L334 320 C300 338 212 338 178 320 Z" fill="{WHITE}"/>
    <!-- gloss -->
    <path d="M110 210 C150 110 290 70 390 140 C300 108 190 130 128 222 Z" fill="#ffffff" opacity="0.28"/>
  </g>'''


DEFS = f'''
  <defs>
    <linearGradient id="rim" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ff5a3c"/><stop offset="0.5" stop-color="{RED}"/><stop offset="1" stop-color="{RED_DARK}"/>
    </linearGradient>
    <radialGradient id="field" cx="0.5" cy="0.4" r="0.6">
      <stop offset="0" stop-color="#8a1018"/><stop offset="1" stop-color="{MAROON}"/>
    </radialGradient>
  </defs>'''

GAME = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">{DEFS}{badge(256, 256, 1.12)}
</svg>
'''


def flame_edge(cx, cy, rx, ry, n, inner):
    """An ellipse with n curved flame tongues sweeping out from the middle."""
    def at(a, f):
        return cx + rx * f * math.cos(a), cy + ry * f * math.sin(a)
    x, y = at(0, inner)
    d = [f"M{x:.1f} {y:.1f}"]
    for k in range(n):
        a0, a1, a2 = (math.tau * (k + t) / n for t in (0, 0.6, 1))
        side = 1 if math.cos(a1) >= 0 else -1
        tx, ty = at(a1, 1.0)
        tx += side * 30
        # Bulge out along the leading edge, hook back along the trailing one.
        c1x, c1y = at(a0 + (a1 - a0) * 0.3, 0.98)
        c2x, c2y = at(a1 + (a2 - a1) * 0.2, 0.8)
        ex, ey = at(a2, inner)
        d.append(f"Q{c1x:.1f} {c1y:.1f} {tx:.1f} {ty:.1f} Q{c2x:.1f} {c2y:.1f} {ex:.1f} {ey:.1f}")
    return " ".join(d) + " Z"


WORDMARK = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 720 210">{DEFS}
  <path d="{flame_edge(360, 112, 330, 92, 16, 0.7)}" fill="url(#rim)" stroke="{MAROON}" stroke-width="5" stroke-linejoin="round"/>
  <path d="{flame_edge(360, 114, 300, 72, 16, 0.84)}" fill="{MAROON}"/>
  {badge(360, 40, 0.15)}
  <g font-family="Noto Sans" font-weight="900" font-style="italic" font-size="66" text-anchor="middle" letter-spacing="3">
    <text x="372" y="140" fill="none" stroke="{MAROON}" stroke-width="14" stroke-linejoin="round" transform="skewX(-6)">DIRT FREEKS</text>
    <text x="372" y="140" fill="{WHITE}" stroke="{RED_DARK}" stroke-width="2" transform="skewX(-6)">DIRT FREEKS</text>
  </g>
</svg>
'''


def svg_png(src, out, w, h):
    subprocess.run(["rsvg-convert", "-w", str(w), "-h", str(h), str(src), "-o", str(out)], check=True)


icons = ROOT / "docs/icons"
game_out = ROOT / "freekstyle/assets/icon"
launcher_out = ROOT / "freekstyle/assets/launcher"

(icons / "game.svg").write_text(GAME)
for s in SIZES:
    svg_png(icons / "game.svg", game_out / f"{s}.png", s, s)

# The launcher disc: crop it out of its black backdrop, round it off and
# punch the centre hole.
src = icons / "launcher-disc-source.png"
disc = icons / "launcher-disc.png"
cx, cy, r, hole = 688, 386, 336, 44
subprocess.run(["magick", str(src), "-crop", f"{2 * r}x{2 * r}+{cx - r}+{cy - r}", "+repage",
                "(", "-size", f"{2 * r}x{2 * r}", "xc:black", "-fill", "white", "-draw", f"circle {r},{r} {r},1",
                "-fill", "black", "-draw", f"circle {r},{r} {r},{r - hole}", ")",
                "-alpha", "off", "-compose", "copy_opacity", "-composite", str(disc)], check=True)
for s in SIZES:
    subprocess.run(["magick", str(disc), "-filter", "Lanczos", "-resize", f"{s}x{s}", str(launcher_out / f"{s}.png")], check=True)

(icons / "wordmark.svg").write_text(WORDMARK)
svg_png(icons / "wordmark.svg", launcher_out / "wordmark.png", 720, 210)
