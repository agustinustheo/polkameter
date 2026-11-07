#!/usr/bin/env python3
"""Generate the PolkaMeter logo: a gauge whose needle points into the red zone,
next to a "PolkaMeter" wordmark.

Everything is plain geometry (no fonts, no third-party marks), so the SVG
renders the same everywhere. The background is transparent. Two files are
written, one per colour scheme:

  assets/logo.svg       dark lettering, for light backgrounds
  assets/logo-dark.svg  light lettering, for dark backgrounds

Usage: python3 scripts/make_logo.py
"""
import math
from pathlib import Path

# The drawing spans y = 65..294 of its canvas, plus a margin; the viewBox crops to that band.
TOP, H = 50, 256

PINK = "#E6007A"
RED = "#F2333A"
TRACK = "#8C90A6"  # mid-tone grey, readable on light and dark backgrounds
INK = {"light": "#1C1B29", "dark": "#F4F3F8"}


def f(x):
    return f"{x:.1f}".rstrip("0").rstrip(".")


def polar(cx, cy, r, deg):
    """Point at `deg` degrees, counter-clockwise from 3 o'clock (y grows down)."""
    a = math.radians(deg)
    return cx + r * math.cos(a), cy - r * math.sin(a)


def arc(cx, cy, r, start, end, color, width):
    """Clockwise arc from `start` to `end` degrees, `start` > `end`."""
    x0, y0 = polar(cx, cy, r, start)
    x1, y1 = polar(cx, cy, r, end)
    large = 1 if start - end > 180 else 0
    return (
        f'<path d="M{f(x0)} {f(y0)} A{f(r)} {f(r)} 0 {large} 1 {f(x1)} {f(y1)}" '
        f'fill="none" stroke="{color}" stroke-width="{f(width)}" stroke-linecap="butt"/>'
    )


# ------------------------------------------------------------------ gauge
def gauge(cx, cy, r, ink):
    out = []
    w = 30
    # Track: normal range, warning range, red zone. Small gaps between them.
    out.append(arc(cx, cy, r, 180, 76, TRACK, w))
    out.append(arc(cx, cy, r, 73, 41, PINK, w))
    out.append(arc(cx, cy, r, 38, 0, RED, w))
    # Ticks inside the track.
    for i in range(11):
        deg = 180 - i * 18
        major = i % 2 == 0
        x0, y0 = polar(cx, cy, r - w / 2 - 12, deg)
        x1, y1 = polar(cx, cy, r - w / 2 - (40 if major else 28), deg)
        color = RED if deg < 40 else TRACK
        out.append(
            f'<line x1="{f(x0)}" y1="{f(y0)}" x2="{f(x1)}" y2="{f(y1)}" stroke="{color}" '
            f'stroke-width="{8 if major else 5}" stroke-linecap="round"/>'
        )
    # Needle: a tapered blade from the hub into the red zone.
    deg = 22
    tip = polar(cx, cy, r - 18, deg)
    back = polar(cx, cy, 30, deg + 180)
    side_a = polar(cx, cy, 13, deg + 90)
    side_b = polar(cx, cy, 13, deg - 90)
    pts = " ".join(f"{f(x)},{f(y)}" for x, y in (tip, side_a, back, side_b))
    out.append(f'<polygon points="{pts}" fill="{ink}" stroke="{ink}" stroke-width="4" stroke-linejoin="round"/>')
    # Hub.
    out.append(f'<circle cx="{f(cx)}" cy="{f(cy)}" r="26" fill="{PINK}"/>')
    out.append(f'<circle cx="{f(cx)}" cy="{f(cy)}" r="9" fill="{ink}"/>')
    return out


# -------------------------------------------------------------- lettering
class Pen:
    """Monoline geometric letters. Each method draws one glyph and advances x."""

    def __init__(self, x, baseline, xh, sw, color):
        self.x, self.b, self.xh, self.sw, self.color = x, baseline, xh, sw, color
        self.r = xh / 2 - sw / 2  # bowl radius, measured to the stroke centre
        self.cap = xh * 1.42
        self.asc = xh * 1.5
        self.gap = sw * 0.95
        self.out = []

    # baseline and top of a stroke, inset by half the stroke so round caps sit on the lines
    @property
    def bot(self):
        return self.b - self.sw / 2

    def top(self, height):
        return self.b - height + self.sw / 2

    def _stroke(self, d):
        self.out.append(
            f'<path d="{d}" fill="none" stroke="{self.color}" stroke-width="{f(self.sw)}" '
            f'stroke-linecap="round" stroke-linejoin="round"/>'
        )

    def _bowl(self):
        cx = self.x + self.r + self.sw / 2
        cy = self.b - self.xh / 2
        return cx, cy

    def P(self):
        sx = self.x + self.sw / 2
        t = self.top(self.cap)
        br = self.cap * 0.29  # bowl radius
        bx = sx + br * 0.9
        self._stroke(
            f"M{f(sx)} {f(self.bot)} L{f(sx)} {f(t)} L{f(bx)} {f(t)} "
            f"A{f(br)} {f(br)} 0 0 1 {f(bx)} {f(t + 2 * br)} L{f(sx)} {f(t + 2 * br)}"
        )
        self.x += self.sw + br * 1.9 + self.gap * 0.7

    def M(self):
        w = self.cap * 0.95
        x0 = self.x + self.sw / 2
        x1 = x0 + w
        t = self.top(self.cap)
        mid = (x0 + x1) / 2
        self._stroke(
            f"M{f(x0)} {f(self.bot)} L{f(x0)} {f(t)} L{f(mid)} {f(self.b - self.cap * 0.3)} "
            f"L{f(x1)} {f(t)} L{f(x1)} {f(self.bot)}"
        )
        self.x += w + self.sw + self.gap

    def o(self):
        cx, cy = self._bowl()
        self.out.append(
            f'<circle cx="{f(cx)}" cy="{f(cy)}" r="{f(self.r)}" fill="none" '
            f'stroke="{self.color}" stroke-width="{f(self.sw)}"/>'
        )
        self.x += 2 * self.r + self.sw + self.gap

    def a(self):
        cx, cy = self._bowl()
        self.out.append(
            f'<circle cx="{f(cx)}" cy="{f(cy)}" r="{f(self.r)}" fill="none" '
            f'stroke="{self.color}" stroke-width="{f(self.sw)}"/>'
        )
        sx = cx + self.r
        self._stroke(f"M{f(sx)} {f(self.top(self.xh))} L{f(sx)} {f(self.bot)}")
        self.x += 2 * self.r + self.sw + self.gap

    def e(self):
        cx, cy = self._bowl()
        r = self.r
        x1, y1 = polar(cx, cy, r, -42)
        self._stroke(
            f"M{f(cx - r)} {f(cy)} L{f(cx + r)} {f(cy)} "
            f"A{f(r)} {f(r)} 0 1 0 {f(x1)} {f(y1)}"
        )
        self.x += 2 * r + self.sw + self.gap

    def l(self):
        sx = self.x + self.sw / 2
        self._stroke(f"M{f(sx)} {f(self.top(self.asc))} L{f(sx)} {f(self.bot)}")
        self.x += self.sw + self.gap * 1.1

    def k(self):
        sx = self.x + self.sw / 2
        w = self.xh * 0.62
        self._stroke(f"M{f(sx)} {f(self.top(self.asc))} L{f(sx)} {f(self.bot)}")
        joint_y = self.b - self.xh * 0.42
        self._stroke(
            f"M{f(sx + w)} {f(self.top(self.xh))} L{f(sx + self.sw * 0.4)} {f(joint_y)} "
            f"M{f(sx + w * 0.38)} {f(self.b - self.xh * 0.6)} L{f(sx + w)} {f(self.bot)}"
        )
        self.x += self.sw + w + self.gap

    def t(self):
        w = self.xh * 0.5
        sx = self.x + w / 2
        q = self.xh * 0.28
        self._stroke(
            f"M{f(sx)} {f(self.top(self.asc * 0.9))} L{f(sx)} {f(self.bot - q)} "
            f"Q{f(sx)} {f(self.bot)} {f(sx + q)} {f(self.bot)}"
        )
        y = self.top(self.xh)
        self._stroke(f"M{f(sx - w / 2)} {f(y)} L{f(sx + w / 2)} {f(y)}")
        self.x += w + self.sw + self.gap * 0.6

    def r_(self):
        sx = self.x + self.sw / 2
        w = self.xh * 0.5
        t = self.top(self.xh)
        self._stroke(f"M{f(sx)} {f(t)} L{f(sx)} {f(self.bot)}")
        self._stroke(f"M{f(sx)} {f(self.b - self.xh * 0.38)} Q{f(sx)} {f(t)} {f(sx + w)} {f(t)}")
        self.x += w + self.sw + self.gap

    def write(self, text):
        for ch in text:
            getattr(self, "r_" if ch == "r" else ch)()
        return self


# ------------------------------------------------------------------ build
def build(scheme):
    ink = INK[scheme]
    out = []
    cx, cy, r = 185, 255, 160
    out += gauge(cx, cy, r, ink)

    xh, sw = 104, 25
    base = 268
    x0 = 400
    polka = Pen(x0, base, xh, sw, PINK).write("Polka")
    meter = Pen(polka.x + sw * 0.4, base, xh, sw, ink).write("Meter")
    out += polka.out + meter.out

    width = math.ceil(meter.x + 20)
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{H}" '
        f'viewBox="0 {TOP} {width} {H}" role="img" '
        f'aria-label="PolkaMeter: a gauge whose needle points into the red zone">'
        f"<title>PolkaMeter</title>" + "".join(out) + "</svg>\n"
    )


def main():
    assets = Path(__file__).resolve().parent.parent / "assets"
    assets.mkdir(exist_ok=True)
    for scheme, name in (("light", "logo.svg"), ("dark", "logo-dark.svg")):
        path = assets / name
        path.write_text(build(scheme))
        print(f"wrote {path}")


if __name__ == "__main__":
    main()
