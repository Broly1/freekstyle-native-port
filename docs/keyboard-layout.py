#!/usr/bin/env python3
# Draws docs/keyboard-layout.svg (the default key bindings); render with
#   rsvg-convert -z 2 docs/keyboard-layout.svg -o docs/keyboard-layout.png
# Keep `binds` in step with default_bindings() in crates/freek-config.
import os

U = 64  # key unit in px
G = 6   # gap
binds = {
    "W": ("D-pad ↑", "move"), "A": ("D-pad ←", "move"), "S": ("D-pad ↓", "move"), "D": ("D-pad →", "move"),
    "↑": ("D-pad ↑", "move"), "←": ("D-pad ←", "move"), "↓": ("D-pad ↓", "move"), "→": ("D-pad →", "move"),
    "J": ("A  Cross", "a"), "Space": ("A  Cross", "a"), "Enter": ("A  Select", "a"),
    "K": ("B  Circle", "b"), "U": ("X  Square", "x"), "I": ("Y  Triangle", "y"),
    "P": ("LB  L1", "sh"), "O": ("RB  R1", "sh"), ";": ("LT  L2", "sh"), "L": ("RT  R2", "sh"),
    "T": ("Pause", "sys"), "Backspace": ("Back  Select", "sys"),
    "Esc": ("Quit", "app"), "F11": ("Fullscreen", "app"), "F12": ("Record", "app"),
}
col = {"move": "#e07a2d", "a": "#3fa34d", "b": "#d64541", "x": "#2f6fd1", "y": "#d9a521", "sh": "#7b5cc4", "sys": "#4a5568", "app": "#2c7a7b"}
rows = [
    [("Esc", 1), ("gap", .5), ("F1", 1), ("F2", 1), ("F3", 1), ("F4", 1), ("gap", .25), ("F5", 1), ("F6", 1), ("F7", 1), ("F8", 1), ("gap", .25), ("F9", 1), ("F10", 1), ("F11", 1), ("F12", 1)],
    [("`", 1)] + [(c, 1) for c in "1234567890"] + [("-", 1), ("=", 1), ("Backspace", 2)],
    [("Tab", 1.5)] + [(c, 1) for c in "QWERTYUIOP"] + [("[", 1), ("]", 1), ("\\", 1.5)],
    [("Caps", 1.75)] + [(c, 1) for c in "ASDFGHJKL"] + [(";", 1), ("'", 1), ("Enter", 2.25)],
    [("Shift", 2.25)] + [(c, 1) for c in "ZXCVBNM"] + [(",", 1), (".", 1), ("/", 1), ("Shift", 2.75)],
    [("Ctrl", 1.25), ("Super", 1.25), ("Alt", 1.25), ("Space", 6.25), ("Alt", 1.25), ("Super", 1.25), ("Menu", 1.25), ("Ctrl", 1.25)],
]
X0, Y0 = 40, 110
out = []


def esc(t):
    return t.replace("&", "&amp;").replace("<", "&lt;")


def key(x, y, w, h, name):
    b = binds.get(name)
    fill = col[b[1]] if b else "#2a2d36"
    stroke = "#ffffff33" if b else "#3a3e4a"
    out.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="9" fill="{fill}" stroke="{stroke}" stroke-width="1.5"/>')
    if b:
        out.append(f'<text x="{x + 9}" y="{y + 25}" font-size="20" font-weight="700" fill="#ffffff">{esc(name)}</text>')
        out.append(f'<text x="{x + w / 2}" y="{y + h - 10}" font-size="12.5" font-weight="600" fill="#ffffffdd" text-anchor="middle">{esc(b[0])}</text>')
    else:
        out.append(f'<text x="{x + 9}" y="{y + 24}" font-size="15" fill="#7d8496">{esc(name)}</text>')


y = Y0
for i, r in enumerate(rows):
    x = X0
    for name, w in r:
        if name != "gap":
            key(x, y, w * U + (w - 1) * G, U, name)
        x += w * U + w * G
    y += U + G + (14 if i == 0 else 0)
# Arrow cluster right of the bottom rows.
ax = X0 + 15.5 * (U + G)
ay = Y0 + 14 + 4 * (U + G)
key(ax + (U + G), ay, U, U, "↑")
for j, n in enumerate(["←", "↓", "→"]):
    key(ax + j * (U + G), ay + U + G, U, U, n)
W = int(ax + 3 * (U + G) + 40)
H = int(y + 150)
legend = [("move", "Movement: D-pad and left stick"), ("a", "A / Cross: select"), ("b", "B / Circle"), ("x", "X / Square"), ("y", "Y / Triangle"),
          ("sh", "Shoulders and triggers"), ("sys", "Start (pause) / Back"), ("app", "Window")]
for i, (k, t) in enumerate(legend):
    cx = X0 + (i % 4) * 300
    cy = y + 40 + (i // 4) * 36
    out.append(f'<rect x="{cx}" y="{cy}" width="22" height="22" rx="5" fill="{col[k]}"/>')
    out.append(f'<text x="{cx + 32}" y="{cy + 17}" font-size="16" fill="#d7dbe4">{t}</text>')
svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" font-family="Noto Sans, DejaVu Sans, sans-serif">
<rect width="{W}" height="{H}" fill="#14161c"/>
<text x="{X0}" y="58" font-size="30" font-weight="800" fill="#ffffff">Freekstyle: keyboard controls</text>
<text x="{X0}" y="86" font-size="16" fill="#9aa1b2">Default layout. Keyboard and controller work together; remap in the launcher's Controls tab.</text>
{chr(10).join(out)}
</svg>'''
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "keyboard-layout.svg"), "w").write(svg)
