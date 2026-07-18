#!/usr/bin/env python3
"""Generate Plan of the Hold variants as standalone SVGs.

Each variant: bastioned trace + inner ward with open gate + gold center.
Night ground, azure line, one gold accent — per BRAND.md.
"""
import math
import os
import sys

CX = CY = 32.0
NIGHT = "#10161E"
LINE = "#3D639C"   # sticker azure — reads well on night ground
GOLD = "#C89B3C"

OUT = sys.argv[1] if len(sys.argv) > 1 else "explorations"


def pt(theta_deg, r, cx=CX, cy=CY):
    a = math.radians(theta_deg)
    return (cx + r * math.cos(a), cy - r * math.sin(a))


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


def trace_path(n, theta0, rc, rt, f, s):
    """Bastioned trace: curtain polygon (radius rc), bastion tips at rt,
    necks at fraction f along each edge, shoulders s units out from necks."""
    C = polygon(n, theta0, rc)
    pts = []
    for k in range(n):
        prev, cur, nxt = C[(k - 1) % n], C[k], C[(k + 1) % n]
        mid_prev = lerp(prev, cur, 0.5)
        mid_next = lerp(cur, nxt, 0.5)
        n_prev = unit((mid_prev[0] - CX, mid_prev[1] - CY))
        n_next = unit((mid_next[0] - CX, mid_next[1] - CY))
        neck_in = lerp(cur, prev, f)
        neck_out = lerp(cur, nxt, f)
        tip = add((CX, CY), unit((cur[0] - CX, cur[1] - CY)), rt)
        pts += [neck_in, add(neck_in, n_prev, s), tip,
                add(neck_out, n_next, s), neck_out]
    d = "M" + " L".join(fmt(p) for p in pts) + " Z"
    return d, C


def ward_path(n, theta0, rw, gate_edge, gapw=4.0):
    """Inner ward polygon with an open gate: a gap centered on edge gate_edge."""
    W = polygon(n, theta0, rw)
    a, b = W[gate_edge % n], W[(gate_edge + 1) % n]
    e = unit((b[0] - a[0], b[1] - a[1]))
    mid = lerp(a, b, 0.5)
    g1 = add(mid, e, -gapw / 2)
    g2 = add(mid, e, gapw / 2)
    pts = [g2] + [W[(gate_edge + 1 + i) % n] for i in range(n)] + [g1]
    return "M" + " L".join(fmt(p) for p in pts)


def ravelins(n, theta0, rc, d1=2.0, d2=6.5, b=3.2):
    """Detached triangles guarding each curtain midpoint."""
    C = polygon(n, theta0, rc)
    out = []
    for k in range(n):
        a, c = C[k], C[(k + 1) % n]
        e = unit((c[0] - a[0], c[1] - a[1]))
        mid = lerp(a, c, 0.5)
        nrm = unit((mid[0] - CX, mid[1] - CY))
        b1 = add(add(mid, e, -b), nrm, d1)
        b2 = add(add(mid, e, b), nrm, d1)
        apex = add(mid, nrm, d2)
        out.append(f"M{fmt(b1)} L{fmt(apex)} L{fmt(b2)} Z")
    return " ".join(out)


def scaled(d_pts_poly, factor):
    """Scale a list of points about center."""
    return [add((CX, CY), (p[0] - CX, p[1] - CY), factor) for p in d_pts_poly]


def svg(name, body, label):
    head = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" '
            f'role="img" aria-label="Plan of the Hold — {label}">\n'
            f'  <rect width="64" height="64" fill="{NIGHT}"/>\n')
    with open(os.path.join(OUT, name), "w") as fh:
        fh.write(head + body + "</svg>\n")
    print(name)


def stroke(d, w, color=LINE):
    return (f'  <path fill="none" stroke="{color}" stroke-width="{w}" '
            f'stroke-linejoin="miter" d="{d}"/>\n')


def dot(r=3.0):
    return f'  <circle fill="{GOLD}" cx="32" cy="32" r="{r}"/>\n'


os.makedirs(OUT, exist_ok=True)

# A — current square (baseline, as shipped on the card/banner)
d, _ = trace_path(4, 45, 16.97, 25.46, 1 / 6, 4.0)
ward = ward_path(4, 45, 9.9, 2)  # edge 2 = bottom
svg("a-square-baseline.svg", stroke(d, 2.2) + stroke(ward, 1.5) + dot(), "square baseline")

# B — pentagon, tip up (the canonical star-fort plan)
d, _ = trace_path(5, 90, 16.0, 26.0, 0.22, 3.2)
ward = ward_path(5, 90, 9.2, 2)  # edge between 234° and 306° corners = bottom
svg("b-pentagon.svg", stroke(d, 2.2) + stroke(ward, 1.5) + dot(), "pentagon")

# C — hexagon, flat top/bottom
d, _ = trace_path(6, 60, 16.5, 25.0, 0.25, 2.8)
ward = ward_path(6, 60, 9.2, 3)  # bottom edge
svg("c-hexagon.svg", stroke(d, 2.2) + stroke(ward, 1.5) + dot(), "hexagon")

# D — square in diamond stance
d, _ = trace_path(4, 90, 16.97, 26.0, 1 / 6, 4.0)
ward = ward_path(4, 90, 9.9, 2)
svg("d-diamond.svg", stroke(d, 2.2) + stroke(ward, 1.5) + dot(), "diamond")

# E — square with ravelins
d, _ = trace_path(4, 45, 15.5, 23.0, 1 / 6, 3.6)
ward = ward_path(4, 45, 8.8, 2)
rav = ravelins(4, 45, 15.5)
svg("e-square-ravelins.svg",
    stroke(d, 2.0) + stroke(rav, 1.2) + stroke(ward, 1.4) + dot(2.8),
    "square with ravelins")

# F — pentagon with moat (double trace)
d, _ = trace_path(5, 90, 14.5, 23.5, 0.22, 3.0)
dm, _ = trace_path(5, 90, 14.5 * 1.24, 23.5 * 1.24, 0.22, 3.0 * 1.24)
ward = ward_path(5, 90, 8.4, 2)
svg("f-pentagon-moat.svg",
    stroke(dm, 0.8) + stroke(d, 2.0) + stroke(ward, 1.4) + dot(2.8),
    "pentagon with moat")
