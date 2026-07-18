#!/usr/bin/env python3
"""Round 2: iterate between B (pentagon) and F (pentagon + moat)."""
import math
import os

CX = CY = 32.0
NIGHT = "#10161E"
LINE = "#3D639C"
GOLD = "#C89B3C"
OUT = "round2"


def pt(theta_deg, r):
    a = math.radians(theta_deg)
    return (CX + r * math.cos(a), CY - r * math.sin(a))


def unit(v):
    m = math.hypot(*v)
    return (v[0] / m, v[1] / m)


def lerp(a, b, t):
    return (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t)


def add(a, b, s=1.0):
    return (a[0] + b[0] * s, a[1] + b[1] * s)


def fmt(p):
    return f"{p[0]:.2f} {p[1]:.2f}"


def polygon(n, theta0, r):
    return [pt(theta0 + k * 360.0 / n, r) for k in range(n)]


def trace_points(n, theta0, rc, rt, f, s):
    """Ordered boundary points. Curtain edge j runs from index 5j+4 to 5(j+1)."""
    C = polygon(n, theta0, rc)
    pts = []
    for k in range(n):
        prev, cur, nxt = C[(k - 1) % n], C[k], C[(k + 1) % n]
        n_prev = unit(add(lerp(prev, cur, 0.5), (-CX, -CY)))
        n_next = unit(add(lerp(cur, nxt, 0.5), (-CX, -CY)))
        neck_in = lerp(cur, prev, f)
        neck_out = lerp(cur, nxt, f)
        tip = add((CX, CY), unit((cur[0] - CX, cur[1] - CY)), rt)
        pts += [neck_in, add(neck_in, n_prev, s), tip,
                add(neck_out, n_next, s), neck_out]
    return pts


def closed(pts):
    return "M" + " L".join(fmt(p) for p in pts) + " Z"


def with_causeway(pts, n, gap_edge, gapw=4.0):
    """Open the closed trace with a gap centered on curtain edge gap_edge."""
    a = pts[5 * gap_edge + 4]
    b = pts[(5 * (gap_edge + 1)) % len(pts)]
    e = unit((b[0] - a[0], b[1] - a[1]))
    mid = lerp(a, b, 0.5)
    g1, g2 = add(mid, e, -gapw / 2), add(mid, e, gapw / 2)
    start = (5 * (gap_edge + 1)) % len(pts)
    order = [pts[(start + i) % len(pts)] for i in range(len(pts))]
    return "M" + fmt(g2) + " L" + " L".join(fmt(p) for p in order) + " L" + fmt(g1)


def ward(n, theta0, rw, gate_edge, gapw=4.0):
    W = polygon(n, theta0, rw)
    a, b = W[gate_edge % n], W[(gate_edge + 1) % n]
    e = unit((b[0] - a[0], b[1] - a[1]))
    mid = lerp(a, b, 0.5)
    g1, g2 = add(mid, e, -gapw / 2), add(mid, e, gapw / 2)
    pts = [g2] + [W[(gate_edge + 1 + i) % n] for i in range(n)] + [g1]
    return "M" + " L".join(fmt(p) for p in pts)


def counterscarp(n, theta0, rc, rt, f, s, d=3.2, trim=0.15):
    """Quiet detached wall fragments opposite each curtain."""
    pts = trace_points(n, theta0, rc, rt, f, s)
    segs = []
    for j in range(n):
        a = pts[5 * j + 4]
        b = pts[(5 * (j + 1)) % len(pts)]
        mid = lerp(a, b, 0.5)
        nrm = unit((mid[0] - CX, mid[1] - CY))
        c1 = add(lerp(a, b, trim), nrm, d)
        c2 = add(lerp(a, b, 1 - trim), nrm, d)
        segs.append(f"M{fmt(c1)} L{fmt(c2)}")
    return " ".join(segs)


def ravelins(n, theta0, rc, rt, f, s, d1=2.0, d2=6.0, b=2.8):
    pts = trace_points(n, theta0, rc, rt, f, s)
    out = []
    for j in range(n):
        a = pts[5 * j + 4]
        c = pts[(5 * (j + 1)) % len(pts)]
        e = unit((c[0] - a[0], c[1] - a[1]))
        mid = lerp(a, c, 0.5)
        nrm = unit((mid[0] - CX, mid[1] - CY))
        b1 = add(add(mid, e, -b), nrm, d1)
        b2 = add(add(mid, e, b), nrm, d1)
        apex = add(mid, nrm, d2)
        out.append(f"M{fmt(b1)} L{fmt(apex)} L{fmt(b2)} Z")
    return " ".join(out)


def svg(name, body, label):
    head = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" '
            f'role="img" aria-label="Plan of the Hold — {label}">\n'
            f'  <rect width="64" height="64" fill="{NIGHT}"/>\n')
    with open(os.path.join(OUT, name), "w") as fh:
        fh.write(head + body + "</svg>\n")
    print(name)


def stroke(d, w):
    return (f'  <path fill="none" stroke="{LINE}" stroke-width="{w}" '
            f'stroke-linejoin="miter" d="{d}"/>\n')


def dot(r=3.0):
    return f'  <circle fill="{GOLD}" cx="32" cy="32" r="{r}"/>\n'


os.makedirs(OUT, exist_ok=True)

P5 = dict(n=5, theta0=90.0)
# Pentagon corners: k=0..4 at 90,162,234,306,18. Bottom curtain = edge 2 (234->306).

# B — reference (round 1 geometry)
tp = trace_points(rc=16.0, rt=26.0, f=0.22, s=3.2, **P5)
svg("b0-reference.svg", stroke(closed(tp), 2.2) + stroke(ward(5, 90, 9.2, 2), 1.5) + dot(),
    "pentagon reference")

# B1 — bolder: heavier trace, larger gold, slightly deeper ward
tp = trace_points(rc=16.0, rt=26.5, f=0.22, s=3.2, **P5)
svg("b1-bolder.svg", stroke(closed(tp), 2.7) + stroke(ward(5, 90, 9.6, 2), 1.8) + dot(3.6),
    "pentagon bolder")

# B2 — counterscarp fragments: quiet detached walls opposite each curtain
geom = dict(rc=15.0, rt=24.5, f=0.22, s=3.0)
tp = trace_points(**geom, **P5)
svg("b2-counterscarp.svg",
    stroke(closed(tp), 2.2) + stroke(counterscarp(**geom, **P5, d=3.4, trim=0.12), 1.0)
    + stroke(ward(5, 90, 8.8, 2), 1.5) + dot(2.9),
    "pentagon with counterscarp")

# B3 — pentagon with ravelins
geom = dict(rc=15.0, rt=24.0, f=0.22, s=3.0)
tp = trace_points(**geom, **P5)
svg("b3-ravelins.svg",
    stroke(closed(tp), 2.0) + stroke(ravelins(**geom, **P5, d1=2.2, d2=6.4, b=2.9), 1.2)
    + stroke(ward(5, 90, 8.6, 2), 1.4) + dot(2.8),
    "pentagon with ravelins")

# F0 — reference (round 1 moat, uniform scale)
d_in = closed(trace_points(rc=14.5, rt=23.5, f=0.22, s=3.0, **P5))
d_moat = closed(trace_points(rc=14.5 * 1.24, rt=23.5 * 1.24, f=0.22, s=3.0 * 1.24, **P5))
svg("f0-reference.svg", stroke(d_moat, 0.8) + stroke(d_in, 2.0) + stroke(ward(5, 90, 8.4, 2), 1.4) + dot(2.8),
    "pentagon moat reference")

# F1 — causeway moat: true offset (constant gap), thinner, opened at the gate
d_in = closed(trace_points(rc=14.0, rt=22.5, f=0.22, s=3.0, **P5))
moat_pts = trace_points(rc=17.4, rt=25.9, f=0.22, s=3.0, **P5)
svg("f1-causeway.svg",
    stroke(with_causeway(moat_pts, 5, 2, gapw=4.6), 0.9)
    + stroke(d_in, 2.1) + stroke(ward(5, 90, 8.2, 2), 1.4) + dot(2.8),
    "pentagon moat with causeway")
