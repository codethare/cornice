#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Rebuild testing/*.png from the PPMs that scripts/live/run.sh and demo.sh leave behind.
# Stdlib only (no PIL): PPM in, PNG out, so the artifacts in testing/ are reproducible rather than
# one-off screenshots nobody can regenerate.
#
#   cargo build && bash scripts/live/run.sh && bash scripts/live/demo.sh
#   python3 scripts/live/artifacts.py                 # -> testing/*.png
#   python3 scripts/live/artifacts.py --live /tmp/x --demo /var/tmp/y --out /tmp/z

import argparse
import struct
import sys
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

# (output, source, crop) — crop is (x, y, w, h, scale) or None for the whole frame
FULL = [
    ("bar.png", "bar"),
    ("exclusive-zone-window.png", "win"),
    ("notification-short-card.png", "short"),
    ("notification-long-card.png", "long"),
    ("notification-two-cards.png", "two"),
    ("notification-critical-urgency.png", "nt3"),
    ("notification-stack-max4.png", "stack"),
    ("notification-action-button.png", "action"),
    ("click-through.png", "through"),
    ("multi-output-1.png", "mo1n"),
    ("multi-output-2.png", "mo2n"),
]
DETAIL = [
    ("detail-bar-right-5x.png", "bar", (1080, 0, 200, 32, 5)),
    ("detail-card-action-4x.png", "action", (1150, 30, 130, 100, 4)),
    ("detail-four-cards-3x.png", "stack", (940, 30, 340, 340, 3)),
]
# Enter filmstrip: independent cards materialising below the bar.
STRIP = ("detail-card-enter-filmstrip.png", [18, 20, 22, 24, 26, 28, 31], (1150, 30, 130, 100, 3))


def read_ppm(path):
    data = Path(path).read_bytes()
    if not data.startswith(b"P6"):
        raise SystemExit(f"{path}: not a P6 PPM")
    fields, i = [], 2
    while len(fields) < 3:
        while data[i : i + 1].isspace():
            i += 1
        if data[i : i + 1] == b"#":
            while data[i : i + 1] != b"\n":
                i += 1
            continue
        j = i
        while not data[j : j + 1].isspace():
            j += 1
        fields.append(int(data[i:j]))
        i = j
    w, h, _ = fields
    i += 1
    return w, h, data[i : i + w * h * 3]


def write_png(path, w, h, rgb):
    def chunk(tag, payload):
        return struct.pack(">I", len(payload)) + tag + payload + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)

    rows = b"".join(b"\x00" + bytes(rgb[y * w * 3 : (y + 1) * w * 3]) for y in range(h))
    Path(path).write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(rows, 9))
        + chunk(b"IEND", b"")
    )


def crop_rows(src, x0, y0, cw, ch, scale):
    """Returns the cropped region as scaled RGB rows (nearest neighbour)."""
    w, h, buf = read_ppm(src)
    out = bytearray()
    for y in range(ch):
        row = bytearray()
        for x in range(cw):
            sx, sy = x0 + x, y0 + y
            if 0 <= sx < w and 0 <= sy < h:
                o = (sy * w + sx) * 3
                row += bytes((buf[o], buf[o + 1], buf[o + 2])) * scale
            else:
                row += bytes(scale * 3)
        for _ in range(scale):
            out += row
    return cw * scale, ch * scale, out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--live", default="/tmp/cornice-live", help="run.sh scratch dir ($W)")
    ap.add_argument("--demo", default="/var/tmp/cornice-live-demo", help="demo.sh scratch dir ($W)")
    ap.add_argument("--out", default=str(ROOT / "testing"))
    a = ap.parse_args()
    live, demo, out = Path(a.live), Path(a.demo), Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    written = 0

    for name, src in FULL:
        p = live / f"{src}.ppm"
        if not p.exists():
            print(f"  skip {name} ({p} missing)")
            continue
        w, h, buf = read_ppm(p)
        write_png(out / name, w, h, buf)
        written += 1

    for name, src, (x0, y0, cw, ch, scale) in DETAIL:
        p = live / f"{src}.ppm"
        if not p.exists():
            print(f"  skip {name} ({p} missing)")
            continue
        w, h, buf = crop_rows(p, x0, y0, cw, ch, scale)
        write_png(out / name, w, h, buf)
        written += 1

    name, frames, (x0, y0, cw, ch, scale) = STRIP
    panels = [demo / "frames" / f"f{i:04d}.ppm" for i in frames]
    if all(p.exists() for p in panels):
        pw, ph = cw * scale, ch * scale
        gap = scale
        total_w, total_h = pw, len(panels) * (ph + gap)
        strip = bytearray()
        for p in panels:
            _, _, buf = crop_rows(p, x0, y0, cw, ch, scale)
            strip += buf
            strip += bytes((0x3C, 0x3C, 0x3C)) * (pw * gap)
        write_png(out / name, total_w, total_h, strip)
        written += 1
    else:
        print(f"  skip {name} ({demo}/frames missing — run demo.sh)")

    gif = demo / "cornice-live.gif"
    if gif.exists():
        (out / gif.name).write_bytes(gif.read_bytes())
        written += 1
    else:
        print(f"  skip {gif.name} ({gif} missing — run demo.sh)")

    print(f"{written} artifact(s) in {out}")
    return 0


sys.exit(main())
