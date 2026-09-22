#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Pixel helpers for the cornice live test. Reads grim's PPM output. Stdlib only.
#
#   shot.py px    FILE X Y              -> "r g b"
#   shot.py bbox  FILE X0 Y0 X1 Y1 [bg]   -> "minx miny maxx maxy count"
#                                          bounding box of pixels that differ from bg
#                                          (bg defaults to 0,0,0 = river's background;
#                                           coordinates are printed in screen space, -1 -1 -1 -1 0 when nothing differs)
#   shot.py near  FILE X0 Y0 X1 Y1 R G B [tol] -> same, for pixels within tol of a colour
#   shot.py bands FILE X Y0 Y1 [bg]      -> number of contiguous non-bg runs in the column X

import sys


def read_ppm(path):
    with open(path, "rb") as f:
        data = f.read()
    if not data.startswith(b"P6"):
        raise SystemExit(f"{path}: not a P6 PPM")
    fields, i = [], 2
    while len(fields) < 3:
        while i < len(data) and data[i : i + 1].isspace():
            i += 1
        if data[i : i + 1] == b"#":
            while data[i : i + 1] != b"\n":
                i += 1
            continue
        j = i
        while j < len(data) and not data[j : j + 1].isspace():
            j += 1
        fields.append(int(data[i:j]))
        i = j
    w, h, _ = fields
    i += 1
    return w, h, data[i : i + w * h * 3]


def pixel(buf, w, x, y):
    o = (y * w + x) * 3
    return buf[o], buf[o + 1], buf[o + 2]


def main():
    cmd, path = sys.argv[1], sys.argv[2]
    w, h, buf = read_ppm(path)
    if cmd == "px":
        x, y = int(sys.argv[3]), int(sys.argv[4])
        print(*pixel(buf, w, x, y))
        return
    if cmd == "size":
        print(w, h)
        return
    if cmd == "bands":
        x = int(sys.argv[3])
        y0, y1 = int(sys.argv[4]), min(int(sys.argv[5]), h)
        bg = tuple(int(a) for a in sys.argv[6].split(",")) if len(sys.argv) > 6 else (0, 0, 0)
        runs, inside = 0, False
        for y in range(y0, y1):
            hit = max(abs(pixel(buf, w, x, y)[k] - bg[k]) for k in range(3)) > 8
            if hit and not inside:
                runs += 1
            inside = hit
        print(runs)
        return
    x0, y0, x1, y1 = (int(a) for a in sys.argv[3:7])
    if cmd == "near":
        ref = tuple(int(a) for a in sys.argv[7:10])
        tol = int(sys.argv[10]) if len(sys.argv) > 10 else 8
    else:
        ref = tuple(int(a) for a in sys.argv[7].split(",")) if len(sys.argv) > 7 else (0, 0, 0)
        tol = 8
    x1, y1 = min(x1, w), min(y1, h)
    minx, miny, maxx, maxy, count = w, h, -1, -1, 0
    for y in range(y0, y1):
        for x in range(x0, x1):
            p = pixel(buf, w, x, y)
            hit = (max(abs(p[k] - ref[k]) for k in range(3)) <= tol) if cmd == "near" \
                else (max(abs(p[k] - ref[k]) for k in range(3)) > tol)
            if hit:
                count += 1
                minx, miny = min(minx, x), min(miny, y)
                maxx, maxy = max(maxx, x), max(maxy, y)
    print(minx, miny, maxx, maxy, count)


main()
