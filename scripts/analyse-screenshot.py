#!/usr/bin/env python3
"""Decide whether a screenshot shows a real, rendered UI or a blank window.

A Tauri app whose WebKit web process failed to start still produces a window —
it is just empty. So "the process is alive" and even "a window exists" are both
too weak to mean the app works. This looks at the pixels instead.

Reads a PPM (what `import`/`xwd | convert` produce without needing PIL) and
reports colour diversity, edge density and the largest single-colour run.

Usage: analyse-screenshot.py shot.ppm [--min-colours N] [--region x,y,w,h]
"""
import sys
from collections import Counter


def read_ppm(path):
    with open(path, "rb") as fh:
        data = fh.read()
    if not data.startswith(b"P6"):
        raise SystemExit(f"{path}: not a binary PPM (P6)")
    # Header: P6 <w> <h> <maxval>, with comments allowed between tokens.
    tokens, i = [], 2
    while len(tokens) < 3:
        while i < len(data) and data[i : i + 1].isspace():
            i += 1
        if data[i : i + 1] == b"#":
            while data[i : i + 1] not in (b"\n", b""):
                i += 1
            continue
        start = i
        while i < len(data) and not data[i : i + 1].isspace():
            i += 1
        tokens.append(int(data[start:i]))
    i += 1
    w, h, _maxval = tokens
    return w, h, data[i : i + w * h * 3]


def analyse(path, region=None):
    w, h, px = read_ppm(path)
    x0, y0, x1, y1 = region or (0, 0, w, h)
    x1, y1 = min(x1, w), min(y1, h)

    counts = Counter()
    rows = []
    for y in range(y0, y1):
        base = y * w * 3
        row = []
        for x in range(x0, x1):
            o = base + x * 3
            c = (px[o], px[o + 1], px[o + 2])
            counts[c] += 1
            row.append(c)
        rows.append(row)

    total = sum(counts.values()) or 1
    dominant, dominant_n = counts.most_common(1)[0]

    # Edges: adjacent pixels differing appreciably. A gradient-filled but
    # content-free window scores low here; real UI chrome scores high.
    edges = 0
    for row in rows:
        for a, b in zip(row, row[1:]):
            if abs(a[0] - b[0]) + abs(a[1] - b[1]) + abs(a[2] - b[2]) > 24:
                edges += 1
    comparisons = max(1, sum(max(0, len(r) - 1) for r in rows))

    return {
        "size": f"{w}x{h}",
        "region": f"{x0},{y0} {x1 - x0}x{y1 - y0}",
        "colours": len(counts),
        "dominant": dominant,
        "dominant_pct": 100.0 * dominant_n / total,
        "edge_pct": 100.0 * edges / comparisons,
    }


def main():
    args = sys.argv[1:]
    if not args:
        raise SystemExit(__doc__)
    path = args[0]
    # Calibrated against two real captures on this app: SvelteKit's error page
    # scores 330 colours / 0.64% edges, while the actual UI scores 1447 / 2.80%.
    # The original 200/0.5% thresholds passed the error page, which made the
    # check worse than useless — it reported success on a dead app.
    min_colours = 600
    min_edges = 1.5
    region = None
    for i, a in enumerate(args):
        if a == "--min-colours":
            min_colours = int(args[i + 1])
        if a == "--min-edges":
            min_edges = float(args[i + 1])
        if a == "--region":
            x, y, rw, rh = (int(v) for v in args[i + 1].split(","))
            region = (x, y, x + rw, y + rh)

    r = analyse(path, region)
    print(f"  image      {r['size']}  region {r['region']}")
    print(f"  colours    {r['colours']}")
    print(f"  dominant   rgb{r['dominant']} covering {r['dominant_pct']:.1f}%")
    print(f"  edges      {r['edge_pct']:.2f}% of adjacent pixel pairs")

    problems = []
    if r["colours"] < min_colours:
        problems.append(f"only {r['colours']} distinct colours (< {min_colours})")
    if r["dominant_pct"] > 97.0:
        problems.append(f"one colour covers {r['dominant_pct']:.1f}% of the region")
    if r["edge_pct"] < min_edges:
        problems.append(
            f"only {r['edge_pct']:.2f}% edges (< {min_edges}%) — too sparse to be the real UI"
        )

    if problems:
        print("  VERDICT    BLANK / NOT RENDERED")
        for p in problems:
            print(f"             - {p}")
        return 1
    print("  VERDICT    RENDERED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
