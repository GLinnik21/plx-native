"""Read-out glyphs: base (what is involved) + badge (what is wrong), baked to plain filled paths.
nanosvg (src/svg.c) has no <mask>/<clipPath>, so the knockout and the badge's cut-through mark are
computed here as polygon booleans and emitted as one even-odd path per icon.

Regenerate with: python3 tools/readout-glyphs.py assets/icons"""
import math, re, sys, os
from shapely.geometry import LineString, LinearRing, Point, Polygon, MultiPolygon
from shapely.ops import unary_union

HW = 0.9            # stroke 1.8 on the 24 grid — the read-out weight
BC = (18.5, 18.5)   # badge centre
KNOCK = 6.6         # base is cut back to this radius
BADGE = 5.0         # badge disc radius

def arc_center(x1, y1, rx, ry, phi, fa, fs, x2, y2):
    cp, sp = math.cos(phi), math.sin(phi)
    dx, dy = (x1 - x2) / 2, (y1 - y2) / 2
    x1p, y1p = cp*dx + sp*dy, -sp*dx + cp*dy
    lam = (x1p**2)/(rx**2) + (y1p**2)/(ry**2)
    if lam > 1: rx, ry = rx*math.sqrt(lam), ry*math.sqrt(lam)
    num = rx*rx*ry*ry - rx*rx*y1p*y1p - ry*ry*x1p*x1p
    den = rx*rx*y1p*y1p + ry*ry*x1p*x1p
    co = math.sqrt(max(0, num/den)) * (-1 if fa == fs else 1)
    cxp, cyp = co*rx*y1p/ry, -co*ry*x1p/rx
    cx = cp*cxp - sp*cyp + (x1 + x2)/2
    cy = sp*cxp + cp*cyp + (y1 + y2)/2
    ang = lambda ux, uy, vx, vy: math.atan2(ux*vy - uy*vx, ux*vx + uy*vy)
    t1 = ang(1, 0, (x1p - cxp)/rx, (y1p - cyp)/ry)
    dt = ang((x1p - cxp)/rx, (y1p - cyp)/ry, (-x1p - cxp)/rx, (-y1p - cyp)/ry)
    if not fs and dt > 0: dt -= 2*math.pi
    if fs and dt < 0: dt += 2*math.pi
    return cx, cy, rx, ry, phi, t1, dt

def parse(d):
    """SVG path -> list of (points, closed). Supports MLHVCAZ, both cases."""
    toks = re.findall(r'[MmLlHhVvCcAaZz]|-?(?:\d+\.?\d*|\.\d+)(?:e-?\d+)?', d)
    i, cmd, cur, start, subs, pts = 0, None, (0, 0), (0, 0), [], []
    def num():
        nonlocal i; v = float(toks[i]); i += 1; return v
    while i < len(toks):
        if re.match(r'[A-Za-z]', toks[i]): cmd = toks[i]; i += 1
        rel = cmd.islower(); c = cmd.upper()
        if c == 'Z':
            subs.append((pts, True)); pts = []; cur = start; continue
        if c == 'M':
            if pts: subs.append((pts, False))
            x, y = num(), num()
            if rel: x, y = cur[0]+x, cur[1]+y
            cur = start = (x, y); pts = [cur]; cmd = 'l' if rel else 'L'; continue
        if c in 'LHV':
            if c == 'L': x, y = num(), num(); x, y = (cur[0]+x, cur[1]+y) if rel else (x, y)
            elif c == 'H': x = num(); x, y = (cur[0]+x if rel else x), cur[1]
            else: y = num(); x, y = cur[0], (cur[1]+y if rel else y)
            cur = (x, y); pts.append(cur); continue
        if c == 'C':
            a = [num() for _ in range(6)]
            if rel: a = [a[k] + cur[k % 2] for k in range(6)]
            p0 = cur; p1, p2, p3 = (a[0], a[1]), (a[2], a[3]), (a[4], a[5])
            for s in range(1, 41):
                t = s/40; u = 1-t
                pts.append((u**3*p0[0]+3*u*u*t*p1[0]+3*u*t*t*p2[0]+t**3*p3[0],
                            u**3*p0[1]+3*u*u*t*p1[1]+3*u*t*t*p2[1]+t**3*p3[1]))
            cur = p3; continue
        if c == 'A':
            rx, ry, rot, fa, fs, x, y = [num() for _ in range(7)]
            if rel: x, y = cur[0]+x, cur[1]+y
            cx, cy, rx, ry, phi, t1, dt = arc_center(cur[0], cur[1], rx, ry, math.radians(rot), int(fa), int(fs), x, y)
            n = max(8, int(abs(dt)*24))
            for s in range(1, n+1):
                t = t1 + dt*s/n
                pts.append((cx + rx*math.cos(t)*math.cos(phi) - ry*math.sin(t)*math.sin(phi),
                            cy + rx*math.cos(t)*math.sin(phi) + ry*math.sin(t)*math.cos(phi)))
            cur = (x, y); continue
        raise ValueError(cmd)
    if pts: subs.append((pts, False))
    return subs

def circ(cx, cy, r):   return f"M{cx-r} {cy}a{r} {r} 0 1 0 {2*r} 0a{r} {r} 0 1 0 {-2*r} 0z"
def ell(cx, cy, rx, ry): return f"M{cx-rx} {cy}a{rx} {ry} 0 1 0 {2*rx} 0a{rx} {ry} 0 1 0 {-2*rx} 0z"
def rrect(x, y, w, h, r):
    return (f"M{x+r} {y}H{x+w-r}a{r} {r} 0 0 1 {r} {r}V{y+h-r}a{r} {r} 0 0 1 {-r} {r}"
            f"H{x+r}a{r} {r} 0 0 1 {-r} {-r}V{y+r}a{r} {r} 0 0 1 {r} {-r}z")

def stroke(d, hw=HW):
    out = []
    for pts, closed in parse(d):
        if closed and len(pts) > 2: out.append(LinearRing(pts).buffer(hw, quad_segs=16))
        elif len(pts) > 1: out.append(LineString(pts).buffer(hw, cap_style='round', join_style='round', quad_segs=16))
    return unary_union(out)
def dot(cx, cy, r): return Point(cx, cy).buffer(r, quad_segs=16)

BASES = {
  "person": lambda: stroke(circ(10, 7.8, 3.7) + "M3.2 20.3c.5-3.9 3.3-6.3 6.8-6.3 1.4 0 2.6.3 3.6.9"),
  "people": lambda: stroke(circ(8.6, 8.4, 3.2) + "M2.6 19.8c.4-3.4 2.8-5.5 6-5.5 1.3 0 2.4.3 3.3.9"
                           + circ(16, 6.6, 2.7) + "M13.4 11.7c.8-.4 1.6-.6 2.6-.6 2.4 0 4.3 1.4 5 3.6"),
  "server": lambda: unary_union([stroke(rrect(3, 3.5, 18, 7, 2) + rrect(3, 13.5, 18, 7, 2)), dot(6.9, 7, 1), dot(6.9, 17, 1)]),
  "globe":  lambda: stroke(circ(12, 12, 9) + ell(12, 12, 3.9, 9) + "M3 12h18"),
  "cloud":  lambda: stroke("M7.2 19.2h9.6a4.3 4.3 0 0 0 .7-8.55 6.2 6.2 0 0 0-11.9 1.7A3.5 3.5 0 0 0 7.2 19.2z"),
  "lock":   lambda: stroke(rrect(4.5, 10.3, 15, 10.7, 2.4) + "M8 10.3V7.6a4 4 0 0 1 8 0v2.7"),
  "clock":  lambda: stroke(circ(12, 12, 9) + "M12 7.2V12l3.1 1.9"),
  "key":    lambda: stroke(circ(7.8, 16.2, 4.2) + "M10.8 13.2 19.6 4.4M16.9 7.1l2.3 2.3M14.8 9.2l1.7 1.7"),
}
MARKS = {  # what is cut THROUGH the badge disc
  "alert":    lambda: unary_union([stroke("M18.5 15.9v3", .85), dot(18.5, 21.1, .95)]),
  "xmark":    lambda: stroke("M16.9 16.9l3.2 3.2M20.1 16.9l-3.2 3.2", .8),
  "question": lambda: unary_union([stroke("M17.1 17.2a1.45 1.45 0 1 1 2 1.35c-.4.2-.6.5-.6.95v.1", .75), dot(18.5, 21.3, .85)]),
  "plus":     lambda: stroke("M18.5 16.2v4.6M16.2 18.5h4.6", .8),
  "minus":    lambda: stroke("M16.2 18.5h4.6", .8),
}

def badged(base, mark):
    b = BASES[base]().difference(Point(*BC).buffer(KNOCK, quad_segs=32))
    k = Point(*BC).buffer(BADGE, quad_segs=32).difference(MARKS[mark]())
    return unary_union([b, k])

def wifi_slash():
    arcs = stroke("M2.4 9a13.6 13.6 0 0 1 19.2 0M5.5 12.4a9.2 9.2 0 0 1 13 0M8.7 15.8a4.7 4.7 0 0 1 6.6 0")
    arcs = unary_union([arcs, dot(12, 19.3, 1.15)])
    slash = LineString([(4, 3.6), (20.4, 20)])
    return unary_union([arcs.difference(slash.buffer(HW + 1.1, cap_style='round')), slash.buffer(HW, cap_style='round')])

def fmt(v): s = f"{v:.2f}".rstrip('0').rstrip('.'); return "0" if s in ("-0", "") else s
def ring(coords):
    c = list(coords)[:-1]
    return "M" + " ".join(f"{fmt(x)} {fmt(y)}" for x, y in c) + "z"
def to_svg(geom):
    geom = geom.simplify(0.045, preserve_topology=True)
    polys = [geom] if isinstance(geom, Polygon) else list(geom.geoms)
    polys = [q for q in polys if q.area > 0.02]  # buffer seams leave zero-area slivers
    d = "".join(ring(p.exterior.coords) + "".join(ring(h.coords) for h in p.interiors if Polygon(h).area > 0.02) for p in polys)
    return f'<svg viewBox="0 0 24 24" fill="#ffffff"><path fill-rule="evenodd" d="{d}"/></svg>\n'

ICONS = {
  "clock-badge-alert": ("clock", "alert"),
  "person-badge-xmark": ("person", "xmark"),
  "server-badge-plus": ("server", "plus"),
  "server-badge-xmark": ("server", "xmark"),
  "server-badge-minus": ("server", "minus"),
  "globe-badge-question": ("globe", "question"),
  "globe-badge-minus": ("globe", "minus"),
  "lock-badge-alert": ("lock", "alert"),
  "cloud-badge-alert": ("cloud", "alert"),
  "key-badge-alert": ("key", "alert"),
  "people-badge-alert": ("people", "alert"),
}
out = sys.argv[1]; os.makedirs(out, exist_ok=True)
for name, (b, m) in ICONS.items():
    open(f"{out}/{name}.svg", "w").write(to_svg(badged(b, m)))
open(f"{out}/wifi-slash.svg", "w").write(to_svg(wifi_slash()))
for f in sorted(os.listdir(out)): print(f, os.path.getsize(f"{out}/{f}"))
