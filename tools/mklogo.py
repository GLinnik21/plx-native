#!/usr/bin/env python3
"""Draw FinNative's two art masters, `assets/logo-master.png` and `assets/splash-master.png`.

    python3 tools/mklogo.py && python3 tools/mkicons.py assets/logo-master.png --splash=assets/splash-master.png

The mark is "FIN" in rounded bars, after PlxNative's PLX: the N's diagonal carries the accent, a
cyan-to-violet gradient, split from its stems by a hairline of background the way PLX's X is split.
The I is a lowercase i whose dot is a small rounded triangle in the same gradient as the N, with a dark play on it.
Every corner is a true fillet computed here, so the masters are exact at any size. `tools/mkicons.py`
then cuts every shipped icon and `pkg/splash.png` from them; this script only draws the masters.
The splash text uses the app's own bundled Inter (`pkg/appfont*.ttf`), so no other font is involved.
"""
import math, sys
from PIL import Image, ImageDraw

S = 4
W = H = 1254
BG = (6, 6, 6)
WHITE = (247, 247, 247)
C0, C1 = (24, 200, 232), (139, 92, 246)
# The i's dot is a rounded triangle of our own, tinted by the same cyan-to-violet gradient field as
# the N's diagonal. Its shape is never Jellyfin's rounded triangle-within-a-triangle, which Jellyfin's
# branding guide reserves for the project itself.

def fillet(poly, r):
    out = []
    n = len(poly)
    for i in range(n):
        P, A, B = poly[i], poly[i - 1], poly[(i + 1) % n]
        ux, uy = A[0] - P[0], A[1] - P[1]; la = math.hypot(ux, uy); ux, uy = ux / la, uy / la
        vx, vy = B[0] - P[0], B[1] - P[1]; lb = math.hypot(vx, vy); vx, vy = vx / lb, vy / lb
        ang = math.acos(max(-1, min(1, ux * vx + uy * vy)))
        rr = r if ang > math.radians(80) else r * 0.55
        t = rr / math.tan(ang / 2)
        lim = 0.45 * min(la, lb)
        if t > lim:
            t = lim; rr = t * math.tan(ang / 2)
        bx, by = ux + vx, uy + vy; bl = math.hypot(bx, by); bx, by = bx / bl, by / bl
        cd = rr / math.sin(ang / 2)
        cx, cy = P[0] + bx * cd, P[1] + by * cd
        p0 = (P[0] + ux * t, P[1] + uy * t); p1 = (P[0] + vx * t, P[1] + vy * t)
        a0 = math.atan2(p0[1] - cy, p0[0] - cx); a1 = math.atan2(p1[1] - cy, p1[0] - cx)
        da = a1 - a0
        while da > math.pi: da -= 2 * math.pi
        while da < -math.pi: da += 2 * math.pi
        steps = 24
        for j in range(steps + 1):
            a = a0 + da * j / steps
            out.append((cx + rr * math.cos(a), cy + rr * math.sin(a)))
    return out

def rect(x0, y0, x1, y1):
    return [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]

def grad(w, h, a, b):
    g = Image.new('RGB', (w, h)); px = g.load(); n = w + h
    for y in range(h):
        for x in range(w):
            t = (x + y) / n
            px[x, y] = tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))
    return g

def logo(out=W):
    top, bot, sw, r, sp = 438, 814, 92, 30, 66
    I_TOP = 585
    x = 214
    white = []
    # F
    white.append(rect(x, top, x + sw, bot))
    white.append(rect(x, top, x + 272, top + sw))
    white.append(rect(x, top + 148, x + 234, top + 148 + sw))
    x += 272 + sp
    # I: a stem under its dot
    white.append(rect(x, I_TOP, x + sw, bot))
    icx = x + sw / 2
    x += sw + sp
    # N
    x0, x1 = x, x + 352
    dyl = bot - top
    dxl = (x1 - x0) - 120
    sec = math.hypot(dxl, dyl) / dyl          # horizontal length per unit of perpendicular width
    k = 106 * sec                              # diagonal's perpendicular weight ~ stem weight
    gap = 13 * sec
    def xl(y):  # left cut line (left edge of diagonal minus gap)
        return (x0 - gap) + (y - top) * ((x1 - k) - x0) / dyl
    def xr(y):  # right cut line
        return (x0 + k + gap) + (y - top) * (x1 - (x0 + k)) / dyl
    def y_on(xfun, xv):  # invert: y where cut line hits x
        return top + (xv - xfun(top)) * dyl / (xfun(bot) - xfun(top))
    left = [(x0, y_on(xl, x0)), (x0 + sw, y_on(xl, x0 + sw)), (x0 + sw, bot), (x0, bot)]
    right = [(x1 - sw, top), (x1, top), (x1, y_on(xr, x1)), (x1 - sw, y_on(xr, x1 - sw))]
    white += [left, right]
    diag = [(x0, top), (x0 + k, top), (x1, bot), (x1 - k, bot)]

    big = Image.new('RGB', (W * S, H * S), BG)
    d = ImageDraw.Draw(big)
    sc = lambda pts: [(px * S, py * S) for px, py in pts]
    for p in white:
        d.polygon(sc(fillet(p, r)), fill=WHITE)
    # one gradient field tints the N's diagonal and the i's dot together, so the dot reads as the
    # start of the same sweep: the field runs from the dot's left edge to the N's right edge
    dcy, hw = 498, 68
    tri = [(icx, dcy - hw * 0.94), (icx + hw, dcy + hw * 0.62), (icx - hw, dcy + hw * 0.62)]
    mask = Image.new('L', big.size, 0)
    md = ImageDraw.Draw(mask)
    md.polygon(sc(fillet(diag, r)), fill=255)
    md.polygon(sc(fillet(tri, 46)), fill=255)
    gx0 = int((icx - hw) * S)
    bx0, by0, bx1, by1 = gx0, top * S, int(x1 * S) + 1, bot * S
    g = grad(256, 256, C0, C1).resize((bx1 - bx0, by1 - by0), Image.BICUBIC)
    big.paste(g, (bx0, by0), mask.crop((bx0, by0, bx1, by1)))
    # the play is centred on the triangle's centroid: a right-pointing triangle's own centroid sits
    # a third of its width from the flat side, so the tip reaches further right than the base left
    pr, pcy = 22, dcy + hw * (0.62 + 0.62 - 0.94) / 3
    play = [(icx - pr * 0.5, pcy - pr * 0.87), (icx + pr, pcy), (icx - pr * 0.5, pcy + pr * 0.87)]
    ImageDraw.Draw(big).polygon(sc(fillet(play, 10)), fill=BG)
    return big.resize((out, out), Image.LANCZOS)

def splash(repo):
    from PIL import ImageChops, ImageFont
    SW, SH = 1920, 1080
    img = Image.new('RGB', (SW, SH), BG)
    big = logo(W * 2)
    ink_mask = ImageChops.difference(big, Image.new('RGB', big.size, BG)).convert('L').point(lambda v: 255 if v > 8 else 0)
    ink = big.crop(ink_mask.getbbox())
    h = 190
    ink = ink.resize((round(ink.width * h / ink.height), h), Image.LANCZOS)
    img.paste(ink, ((SW - ink.width) // 2, 420))
    d = ImageDraw.Draw(img)
    f = ImageFont.truetype(str(repo / 'pkg/appfont-bold.ttf'), 42)
    word, track = 'NATIVE', 34
    ws = [d.textlength(c, font=f) for c in word]
    tw = sum(ws) + track * (len(word) - 1)
    # the NATIVE line and its rules, filled with the mark's own gradient
    m = Image.new('L', (SW, SH), 0)
    md = ImageDraw.Draw(m)
    tx, ty = (SW - tw) / 2, 660
    cx = tx
    for c, w in zip(word, ws):
        md.text((cx, ty), c, font=f, fill=255)
        cx += w + track
    lw, lg, ly = 84, 30, ty + 22
    md.line([(tx - lg - lw, ly), (tx - lg, ly)], fill=255, width=2)
    md.line([(tx + tw + lg, ly), (tx + tw + lg + lw, ly)], fill=255, width=2)
    lo, hi = int(tx - lg - lw), int(tx + tw + lg + lw)
    fill = Image.new('RGB', (SW, SH), BG)
    fill.paste(grad(256, 1, C0, C1).resize((hi - lo, SH)), (lo, 0))
    img.paste(fill, (0, 0), m)
    f2 = ImageFont.truetype(str(repo / 'pkg/appfont.ttf'), 26)
    credit = 'powered by PlxNative'
    d.text(((SW - d.textlength(credit, font=f2)) / 2, 980), credit, font=f2, fill=(120, 120, 124))
    return img

if __name__ == '__main__':
    from pathlib import Path
    repo = Path(__file__).resolve().parent.parent
    logo().save(repo / 'assets/logo-master.png')
    splash(repo).save(repo / 'assets/splash-master.png')
