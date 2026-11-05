#!/usr/bin/env python3
"""Generates the localwispr app icon (data/icons/org.localwispr.Settings.svg).

The mark is a 3.5-period sound wave under a sin^2.4 envelope, with one faint
echo trailing behind it, white on a black rounded tile. The geometry matches
the "localwispr — logo" board in Figma; change it here, not by hand.
"""
import math
import pathlib

SIZE = 256
TILE, TILE_RADIUS = 224, 66.5          # 16 px margin, like GNOME app icons
X0, X1, BASELINE = 42.25, 213.75, 121.9
PERIODS, ENVELOPE = 3.5, 2.4
POINTS = 140
# (amplitude, phase, stroke width, opacity), echo first so the wave draws on top.
# stroke-opacity rather than opacity: SVG Tiny renderers (Qt, so KDE) need it.
WAVES = [(45.5, 0.85, 3.5, 0.38), (57.75, 0.0, 6.125, 1.0)]


def path(amp: float, phase: float) -> str:
    out = []
    for i in range(POINTS + 1):
        t = i / POINTS
        env = math.sin(math.pi * t) ** ENVELOPE
        x = X0 + t * (X1 - X0)
        y = BASELINE - amp * env * math.sin(2 * math.pi * PERIODS * t + phase)
        out.append(f"{'M' if i == 0 else 'L'}{x:.2f} {y:.2f}")
    return "".join(out)


def svg() -> str:
    m = (SIZE - TILE) / 2
    stops = (
        '<stop offset="0" stop-color="#fff" stop-opacity="0"/>'
        '<stop offset=".15" stop-color="#fff"/>'
        '<stop offset=".85" stop-color="#fff"/>'
        '<stop offset="1" stop-color="#fff" stop-opacity="0"/>'
    )
    paths = "\n".join(
        f'<path d="{path(a, p)}" stroke="url(#fade)" stroke-width="{w}"'
        + (f' stroke-opacity="{o}"' if o < 1 else "")
        + "/>"
        for a, p, w, o in WAVES
    )
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{SIZE}" height="{SIZE}" viewBox="0 0 {SIZE} {SIZE}">\n'
        f'<defs><linearGradient id="fade" x1="{X0}" y1="0" x2="{X1}" y2="0" gradientUnits="userSpaceOnUse">{stops}</linearGradient></defs>\n'
        f'<rect x="{m}" y="{m}" width="{TILE}" height="{TILE}" rx="{TILE_RADIUS}" fill="#000"/>\n'
        f'<g fill="none" stroke-linecap="round" stroke-linejoin="round">\n{paths}\n</g>\n'
        "</svg>\n"
    )


if __name__ == "__main__":
    out = pathlib.Path(__file__).resolve().parent.parent / "data/icons/org.localwispr.Settings.svg"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(svg())
    print(out)
