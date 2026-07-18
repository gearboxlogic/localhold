#!/usr/bin/env python3
"""Round 3: B2 counterscarp with a causeway opening at the gate."""
import os
import round2 as r2

r2.OUT = OUT = "round3"
os.makedirs(OUT, exist_ok=True)
P5 = dict(n=5, theta0=90.0)


def counterscarp_causeway(d=3.4, trim=0.12, gate_edge=2, gapw=4.4, **geom):
    """Counterscarp fragments; the gate edge's fragment is split by a causeway."""
    pts = r2.trace_points(**geom, **P5)
    n = P5["n"]
    segs = []
    for j in range(n):
        a = pts[5 * j + 4]
        b = pts[(5 * (j + 1)) % len(pts)]
        mid = r2.lerp(a, b, 0.5)
        nrm = r2.unit((mid[0] - r2.CX, mid[1] - r2.CY))
        c1 = r2.add(r2.lerp(a, b, trim), nrm, d)
        c2 = r2.add(r2.lerp(a, b, 1 - trim), nrm, d)
        if j == gate_edge:
            e = r2.unit((b[0] - a[0], b[1] - a[1]))
            m_off = r2.add(mid, nrm, d)
            g1 = r2.add(m_off, e, -gapw / 2)
            g2 = r2.add(m_off, e, gapw / 2)
            segs.append(f"M{r2.fmt(c1)} L{r2.fmt(g1)} M{r2.fmt(g2)} L{r2.fmt(c2)}")
        else:
            segs.append(f"M{r2.fmt(c1)} L{r2.fmt(c2)}")
    return " ".join(segs)


geom = dict(rc=15.0, rt=24.5, f=0.22, s=3.0)
trace = r2.closed(r2.trace_points(**geom, **P5))

# B2C — counterscarp with causeway, round-2 weight
r2.svg("b2c-causeway.svg",
       r2.stroke(trace, 2.2)
       + r2.stroke(counterscarp_causeway(**geom), 1.0)
       + r2.stroke(r2.ward(5, 90, 8.8, 2), 1.5) + r2.dot(2.9),
       "counterscarp with causeway")

# B2C-bold — same, at B1's confidence: heavier trace and ward, larger gold
r2.svg("b2c-bold.svg",
       r2.stroke(trace, 2.6)
       + r2.stroke(counterscarp_causeway(**geom), 1.1)
       + r2.stroke(r2.ward(5, 90, 9.0, 2), 1.7) + r2.dot(3.4),
       "counterscarp causeway bold")

# B2-open — bottom fragment removed entirely: the whole approach lies open
pts = r2.trace_points(**geom, **P5)
segs = []
for j in range(5):
    if j == 2:
        continue
    a = pts[5 * j + 4]
    b = pts[(5 * (j + 1)) % len(pts)]
    mid = r2.lerp(a, b, 0.5)
    nrm = r2.unit((mid[0] - r2.CX, mid[1] - r2.CY))
    c1 = r2.add(r2.lerp(a, b, 0.12), nrm, 3.4)
    c2 = r2.add(r2.lerp(a, b, 0.88), nrm, 3.4)
    segs.append(f"M{r2.fmt(c1)} L{r2.fmt(c2)}")
r2.svg("b2o-open-approach.svg",
       r2.stroke(trace, 2.2) + r2.stroke(" ".join(segs), 1.0)
       + r2.stroke(r2.ward(5, 90, 8.8, 2), 1.5) + r2.dot(2.9),
       "counterscarp open approach")
