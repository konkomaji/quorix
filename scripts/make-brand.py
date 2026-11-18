# SPDX-License-Identifier: GPL-3.0-or-later
"""Builds assets/brand/banner.svg and assets/brand/logo.svg.

Text is converted to outlines from the Poppins files in theme/fonts, because an
SVG shown through an <img> tag (as on GitHub) cannot load fonts.

Needs: pip install fonttools brotli
Run from the repository root: python scripts/make-brand.py
"""
import math
import os

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

FONTS = {w: TTFont(f"theme/fonts/poppins-{w}.woff2") for w in (400, 500, 600)}

INK = "#141415"
TEXT = "#ededed"
MUTED = "#a1a1a6"
FAINT = "#2e2e31"
ACCENT = "#ff5b2e"
ON_ACCENT = "#1c0a04"


def outline(text, weight, size, x, baseline):
    """Returns (path data, width) for `text` set at `size` px, starting at x on `baseline`."""
    font = FONTS[weight]
    glyphs, cmap, metrics = font.getGlyphSet(), font.getBestCmap(), font["hmtx"]
    scale = size / font["head"].unitsPerEm
    pen = SVGPathPen(glyphs, ntos=lambda v: f"{v:.2f}".rstrip("0").rstrip("."))
    cursor = 0.0
    for ch in text:
        name = cmap[ord(ch)]
        glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, x + cursor * scale, baseline)))
        cursor += metrics[name][0]
    return pen.getCommands(), cursor * scale


def mark(x, y, size):
    """The Quorix mark: a ring with a tail, on an accent tile."""
    s = size / 128
    return (
        f'<rect x="{x}" y="{y}" width="{size}" height="{size}" rx="{32 * s:g}" fill="{ACCENT}"/>'
        f'<circle cx="{x + 58 * s:g}" cy="{y + 58 * s:g}" r="{30 * s:g}" fill="none" stroke="{ON_ACCENT}" stroke-width="{13 * s:g}"/>'
        f'<path d="M{x + 82 * s:g} {y + 82 * s:g}l{20 * s:g} {20 * s:g}" stroke="{ON_ACCENT}" stroke-width="{13 * s:g}" stroke-linecap="round"/>'
    )


def wave(x0, x1, y, wavelength=32, amplitude=7):
    """A wavy line made of whole cycles: the 'bytes are moving' part of the progress bar."""
    cycles = int((x1 - x0) // wavelength)
    half = wavelength / 2
    d = f"M{x0} {y}q{half / 2:g} {-amplitude * 2:g} {half:g} 0"
    d += f"t{half:g} 0" * (cycles * 2 - 1)
    return d, x0 + cycles * wavelength


def byte_map(x, y, width, height=36, workers=8, per_worker=6, progress=0.64):
    """The byte map: one group of cells per connection, each filled from its own start."""
    jitter = [0.08, -0.1, 0.04, -0.06, 0.1, -0.04, 0.06, -0.08]
    gap, group_gap = 3, 9
    cell = (width - (workers * per_worker - 1) * gap - (workers - 1) * (group_gap - gap)) / (workers * per_worker)
    out, cx = [], x
    for k in range(workers):
        full = round(per_worker * min(1, max(0, progress + jitter[k])))
        for j in range(per_worker):
            fill = TEXT if j < full else ACCENT if j == full else FAINT
            out.append(f'<rect x="{cx:.2f}" y="{y}" width="{cell:.2f}" height="{height}" rx="2.5" fill="{fill}"/>')
            cx += cell + gap
        cx += group_gap - gap
    return "".join(out)


def pill(label, x, y, height=40, pad=18):
    d, w = outline(label, 500, 16, x + pad, y + height / 2 + 5.6)
    box = f'<rect x="{x}" y="{y}" width="{w + pad * 2:.1f}" height="{height}" rx="{height / 2}" fill="none" stroke="{FAINT}" stroke-width="1.5"/>'
    return box + f'<path d="{d}" fill="{MUTED}"/>', w + pad * 2


def banner():
    W, H = 1280, 360
    parts = [f'<rect width="{W}" height="{H}" rx="32" fill="{INK}"/>', mark(72, 68, 88)]

    d, _ = outline("Quorix", 600, 76, 184, 139)
    parts.append(f'<path d="{d}" fill="{TEXT}"/>')
    d, _ = outline("Fast by design. Reliable by default.", 400, 26, 72, 214)
    parts.append(f'<path d="{d}" fill="{MUTED}"/>')

    x = 72
    for label in ("Open source", "Download manager", "Rust engine"):
        svg, w = pill(label, x, 252)
        parts.append(svg)
        x += w + 10

    # Right side: the two signature elements of the interface.
    left, right = 800, 1208
    d, wave_end = wave(left, left + (right - left) * 0.64, 128)
    parts.append(f'<path d="{d}" fill="none" stroke="{ACCENT}" stroke-width="6" stroke-linecap="round"/>')
    parts.append(f'<path d="M{wave_end + 14:g} 128H{right - 14}" stroke="{FAINT}" stroke-width="6" stroke-linecap="round"/>')
    parts.append(f'<circle cx="{right}" cy="128" r="3" fill="{ACCENT}"/>')
    parts.append(byte_map(left, 176, right - left))
    d, _ = outline("Every byte accounted for", 400, 16, left, 252)
    parts.append(f'<path d="{d}" fill="{MUTED}"/>')

    title = "Quorix. Fast by design. Reliable by default."
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" aria-label="{title}">'
        f"<title>{title}</title>" + "".join(parts) + "</svg>\n"
    )


def logo():
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" width="128" height="128" role="img" aria-label="Quorix">'
        "<title>Quorix</title>" + mark(0, 0, 128) + "</svg>\n"
    )


if __name__ == "__main__":
    assert math.isclose(FONTS[400]["head"].unitsPerEm, 1000)
    os.makedirs("assets/brand", exist_ok=True)
    for name, content in (("banner.svg", banner()), ("logo.svg", logo())):
        with open(f"assets/brand/{name}", "w", encoding="utf-8", newline="\n") as f:
            f.write(content)
        print("wrote", f"assets/brand/{name}", len(content), "bytes")
