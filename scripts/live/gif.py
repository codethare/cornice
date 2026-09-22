#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Assemble PPM frames into an animated GIF. Stdlib only — this box has no ffmpeg, no ImageMagick, no PIL.
#
#   gif.py --frames DIR --times FILE --out FILE.gif [--colors 128] [--scale N] [--every N] [--check]
#
# Frames are read in filename order; `--times` holds one nanosecond timestamp per frame (the capture
# end time the recorder logged), so frame delays follow the recording instead of an assumed frame rate.
# `--check` decodes what was just written with the decoder in this file and compares it with the pixels
# that went in: a GIF encoder that nobody can decode is the one failure mode worth testing for.
#
# The encoder: median-cut palette shared by all frames, 4-4-4 nearest-colour LUT, GIF LZW.

import argparse
import glob
import os
import struct
import sys
from collections import Counter


def read_ppm(path):
    with open(path, "rb") as f:
        data = f.read()
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


# ---------------------------------------------------------------- palette

def median_cut(hist, want):
    box_pixels = lambda box: sum(hist[p] for p in box)
    def longest(box):
        return max(range(3), key=lambda c: max(p[c] for p in box) - min(p[c] for p in box))
    boxes = [list(hist)]
    while len(boxes) < want:
        # only splittable boxes count: the single busiest colour (the background) must not stop the
        # subdivision while other boxes still hold hundreds of colours
        splittable = [b for b in boxes if len(b) > 1]
        if not splittable:
            break
        box = max(splittable, key=box_pixels)
        ch = longest(box)
        box.sort(key=lambda p: p[ch])
        total, acc, cut = box_pixels(box), 0, 1
        for j, p in enumerate(box):
            acc += hist[p]
            if acc * 2 >= total:
                cut = max(1, j)
                break
        boxes.remove(box)
        boxes += [box[:cut], box[cut:]]
    palette = []
    for box in boxes:
        n = box_pixels(box) or 1
        palette.append(tuple(sum(p[c] * hist[p] for p in box) // n for c in range(3)))
    return palette


BITS = 5                      # 5-5-5 key: cell edge 8, so #1a1a1a and #101010 stay distinguishable
SHRINK = bytes(v >> (8 - BITS) for v in range(256))


def build_lut(palette):
    """5-5-5 RGB key -> palette index, as bytes.

    Keys that are a palette colour's own cell map to it exactly, so flat UI colours survive
    untouched; the rest go to the palette colour nearest the cell centre.
    """
    n = 1 << (3 * BITS)
    shift, half = 8 - BITS, 1 << (8 - BITS - 1)
    lut = bytearray(n)
    exact = {}
    for i, c in enumerate(palette):
        exact[(c[0] >> shift << (2 * BITS)) | (c[1] >> shift << BITS) | (c[2] >> shift)] = i
    for key in range(n):
        if key in exact:
            lut[key] = exact[key]
            continue
        r = ((key >> (2 * BITS)) << shift) | half
        g = (((key >> BITS) & ((1 << BITS) - 1)) << shift) | half
        b = ((key & ((1 << BITS) - 1)) << shift) | half
        best, bestd = 0, 1 << 30
        for i, (pr, pg, pb) in enumerate(palette):
            d = (pr - r) ** 2 + (pg - g) ** 2 + (pb - b) ** 2
            if d < bestd:
                best, bestd = i, d
        lut[key] = best
    return bytes(lut)


def downscale(rgb, w, h, factor):
    """Box-average by an integer factor: grim's own -s scaling costs ~150 ms/frame, this costs 0.4 s
    once per frame after recording, when nobody is waiting."""
    if factor == 1:
        return rgb, w, h
    ow, oh = w // factor, h // factor
    out = bytearray(ow * oh * 3)
    f, n = factor, factor * factor
    for oy in range(oh):
        rows = [rgb[(oy * f + k) * w * 3 : (oy * f + k + 1) * w * 3] for k in range(f)]
        base = oy * ow * 3
        for ox in range(ow):
            o = base + ox * 3
            for c in range(3):
                tot = 0
                for k in range(f):
                    r = rows[k]
                    for j in range(f):
                        tot += r[(ox * f + j) * 3 + c]
                out[o + c] = tot // n
    return bytes(out), ow, oh


def index_frame(rgb, lut):
    r = rgb[0::3].translate(SHRINK)
    g = rgb[1::3].translate(SHRINK)
    b = rgb[2::3].translate(SHRINK)
    return [lut[(rv << (2 * BITS)) | (gv << BITS) | bv] for rv, gv, bv in zip(r, g, b)]


# ---------------------------------------------------------------- LZW

def lzw_encode(indices, min_code_size):
    clear, end = 1 << min_code_size, (1 << min_code_size) + 1
    out, acc, nbits = bytearray(), 0, 0

    def emit(code, size):
        nonlocal acc, nbits
        acc |= code << nbits
        nbits += size
        while nbits >= 8:
            out.append(acc & 0xFF)
            acc >>= 8
            nbits -= 8

    code_size, next_code, table, prev = min_code_size + 1, end + 1, {}, None
    emit(clear, code_size)
    for px in indices:
        if prev is None:
            prev = px
            continue
        key = (prev << 8) | px
        got = table.get(key)
        if got is not None:
            prev = got
            continue
        emit(prev, code_size)
        if next_code == 4096:  # the table is full: clear instead of assigning an out-of-range code
            emit(clear, code_size)
            table, next_code, code_size = {}, end + 1, min_code_size + 1
        else:
            table[key] = next_code
            next_code += 1
            if next_code > (1 << code_size):
                code_size += 1
        prev = px
    emit(prev, code_size)
    emit(end, code_size)
    if nbits:
        out.append(acc & 0xFF)
    return bytes(out)


def lzw_decode(data, min_code_size, count):
    clear, end = 1 << min_code_size, (1 << min_code_size) + 1
    code_size, table = min_code_size + 1, None
    acc, nbits, pos = 0, 0, 0
    out, prev = [], None

    def read_code():
        nonlocal acc, nbits, pos
        while nbits < code_size:
            if pos >= len(data):
                return None
            acc |= data[pos] << nbits
            pos += 1
            nbits += 8
        code = acc & ((1 << code_size) - 1)
        acc >>= code_size
        nbits -= code_size
        return code

    while len(out) < count:
        code = read_code()
        if code is None or code == end:
            break
        if code == clear:
            table = [[i] for i in range(clear)] + [None, None]
            code_size = min_code_size + 1
            prev = None
            continue
        if table is None:
            raise SystemExit("check: LZW data without a leading clear code")
        if code < len(table) and table[code] is not None:
            entry = table[code]
        elif code == len(table) and prev is not None:
            entry = prev + [prev[0]]
        else:
            raise SystemExit(f"check: bad LZW code {code}")
        out.extend(entry)
        if prev is not None:
            table.append(prev + [entry[0]])
            if len(table) >= (1 << code_size) and code_size < 12:
                code_size += 1
        prev = entry
    return out[:count]


# ---------------------------------------------------------------- GIF

def write_gif(path, w, h, frames, delays, palette, check):
    bits = max(2, (len(palette) - 1).bit_length())
    size = 1 << bits
    table = bytearray()
    for i in range(size):
        table.extend(palette[i] if i < len(palette) else (0, 0, 0))

    out = bytearray(b"GIF89a")
    out += struct.pack("<HHBBB", w, h, 0xF0 | (bits - 1), 0, 0)
    out += table
    out += b"\x21\xFF\x0BNETSCAPE2.0\x03\x01\x00\x00\x00"  # loop forever

    for idx, (indices, delay) in enumerate(zip(frames, delays)):
        data = lzw_encode(indices, bits)
        if check is not None:
            if check.get("stop"):
                raise SystemExit("check failed: " + check["error"])
            back = lzw_decode(data, bits, len(indices))
            if back != indices:
                bad = next(i for i, (a, b) in enumerate(zip(back, indices)) if a != b)
                raise SystemExit(f"check failed: frame {idx} pixel {bad}: decoded {back[bad]} != encoded {indices[bad]}")
            check["frames"] = check.get("frames", 0) + 1
        delay = max(2, min(delay, 65535))
        out += bytes([0x21, 0xF9, 4, 0x04]) + struct.pack("<H", delay) + bytes([0, 0])
        out += struct.pack("<BHHHHB", 0x2C, 0, 0, w, h, 0) + bytes([bits])
        for i in range(0, len(data), 255):
            chunk = data[i : i + 255]
            out += bytes([len(chunk)]) + chunk
        out += b"\x00"
    out += b"\x3B"
    with open(path, "wb") as f:
        f.write(out)
    return len(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--frames", required=True)
    ap.add_argument("--times")
    ap.add_argument("--out", required=True)
    ap.add_argument("--colors", type=int, default=128)
    ap.add_argument("--scale", type=int, default=1, help="box-average factor (2 = half size)")
    ap.add_argument("--every", type=int, default=1)
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()

    all_files = sorted(glob.glob(os.path.join(a.frames, "*.ppm")))
    files = all_files[:: a.every]
    if not files:
        raise SystemExit(f"no frames in {a.frames}")

    def load(path):
        w0, h0, rgb = read_ppm(path)
        rgb, w0, h0 = downscale(rgb, w0, h0, a.scale)
        return w0, h0, rgb

    w, h, _ = load(files[0])

    times = []
    if a.times and os.path.exists(a.times):
        times = [int(t) for t in open(a.times).read().split()]
    # the recorder numbers frames in capture order, so timing survives --every (a delay is the real
    # gap between the two frames that are actually kept, not a fixed frame rate)
    index = [int("".join(c for c in os.path.basename(f) if c.isdigit()) or 0) for f in files]
    delays = []
    for k in range(len(files)):
        if len(times) > max(index) and k + 1 < len(files):
            delays.append(max(2, min(round((times[index[k + 1]] - times[index[k]]) / 1e7), 100)))
        else:
            delays.append(7)

    print(f"{len(files)} frames ({files[0].split('/')[-1]}..{files[-1].split('/')[-1]}), every {a.every}", file=sys.stderr)
    # global palette from a sample of frames (all of them is not needed and costs seconds)
    step = max(1, len(files) // 12)
    hist = Counter()
    for f in files[::step]:
        rgb = load(f)[2]
        hist.update(zip(rgb[0::3], rgb[1::3], rgb[2::3]))
    palette = median_cut(hist, a.colors)
    lut = build_lut(palette)

    check = {} if a.check else None
    frames = (index_frame(load(f)[2], lut) for f in files)
    n = write_gif(a.out, w, h, frames, delays, palette, check)
    if a.check:
        print(f"check: decoded {check.get('frames', 0)} frames, pixel-identical", file=sys.stderr)
    print(f"{a.out}: {len(files)} frames, {w}x{h}, {len(palette)} colours, {n / 1e6:.1f} MB")


if __name__ == "__main__":
    main()
