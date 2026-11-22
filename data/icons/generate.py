"""Generates the rclover icons as clean vector paths. Run: python3 data/icons/generate.py data/icons (needs shapely)."""
import sys
from shapely.geometry import Point, LineString
from shapely.ops import unary_union
from shapely import affinity

OUT = sys.argv[1]
ID = "io.github.cyythanapong_netizen.Rclover"

def leaf():
    """A heart: two teardrops (round lobe + shared tip), so the sides run smoothly into the
    lobes and only the notch between the lobes is concave."""
    tip = Point(0, -3).buffer(3, 32)
    halves = [Point(x, -35).buffer(14, 64).union(tip).convex_hull for x in (-10.5, 10.5)]
    return unary_union(halves)

def clover(stem):
    parts = [affinity.rotate(leaf(), a, origin=(0, 0)) for a in (0, 120, 240)] + [Point(0, 0).buffer(8.5, 64)]
    g = unary_union(parts).buffer(3, 32).buffer(-3, 32)  # fillet the leaf joins and soften the notch
    if stem:
        pts = []
        for i in range(25):  # cubic Bezier down the gap between the lower leaves
            t = i / 24
            b = lambda a, c1, c2, e: (1-t)**3*a + 3*(1-t)**2*t*c1 + 3*(1-t)*t**2*c2 + t**3*e
            pts.append((b(0, 0.5, 2.5, 9), b(4, 22, 37, 52)))
        # Added after filleting so it joins only at the centre; round caps, no sharp end.
        g = unary_union([g, LineString(pts).buffer(3.4, 32)])
    return g.simplify(0.04)

def d(geom, k, cx, cy):
    """SVG path data, clover centre (0,0) mapped to (cx, cy) at scale k."""
    polys = getattr(geom, "geoms", [geom])
    f = lambda x, y: f"{x*k+cx:.2f} {y*k+cy:.2f}"
    out = []
    for p in polys:
        for ring in [p.exterior, *p.interiors]:
            c = list(ring.coords)
            out.append("M" + f(*c[0]) + "L" + " ".join(f(*q) for q in c[1:-1]) + "Z")
    return "".join(out)

def centred(geom, k, cx, cy):
    """Path data with the outline's bounding box centred on (cx, cy): the top leaf reaches
    further than the lower ones, so centring on the clover's middle would sit it too high."""
    x0, y0, x1, y1 = geom.bounds
    return d(geom, k, cx - (x0 + x1) / 2 * k, cy - (y0 + y1) / 2 * k)

def reach(geom):
    x0, y0, x1, y1 = geom.bounds
    return max(-x0, -y0, x1, y1)

FRAME = ('<defs><linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#2a2a30"/>'
         '<stop offset="1" stop-color="#121214"/></linearGradient>'
         '<radialGradient id="haze" cx=".2" cy=".1" r=".95"><stop offset="0" stop-color="#2ec27e" stop-opacity=".55"/>'
         '<stop offset=".45" stop-color="#3584e4" stop-opacity=".25"/><stop offset="1" stop-color="#9141ac" stop-opacity="0"/></radialGradient></defs>'
         '<rect x="8" y="8" width="112" height="112" rx="28" fill="url(#bg)"/>'
         '<rect x="8" y="8" width="112" height="112" rx="28" fill="url(#haze)"/>'
         '<rect x="8.5" y="8.5" width="111" height="111" rx="27.5" fill="none" stroke="#fff" stroke-opacity=".16"/>')
MINT = "#8ff0a4"

leaves = clover(False)
k_app = 40 / reach(leaves)      # trefoil radius 40 inside the 112 frame
k_small = 46 / reach(leaves)    # no stem: let the leaves fill more of the frame
k_sym = 63 / reach(leaves)

def svg(body, size=128):
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" width="{size}" height="{size}">{body}</svg>\n'

files = {
    f"hicolor/scalable/apps/{ID}.svg": svg(FRAME + f'<path fill="{MINT}" d="{centred(clover(True), k_app, 64, 65)}"/>'),
    f"hicolor/24x24/apps/{ID}.svg": svg(FRAME + f'<path fill="{MINT}" d="{centred(leaves, k_small, 64, 64)}"/>', 24),
    f"hicolor/16x16/apps/{ID}.svg": svg(FRAME + f'<path fill="{MINT}" d="{centred(leaves, k_small, 64, 64)}"/>', 16),
    f"hicolor/symbolic/apps/{ID}-symbolic.svg": svg(f'<path fill="#2e3436" d="{centred(leaves, k_sym, 64, 64)}"/>', 16),
}
import os
for rel, text in files.items():
    path = os.path.join(OUT, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    open(path, "w").write(text)
    print(rel, len(text), "bytes")
