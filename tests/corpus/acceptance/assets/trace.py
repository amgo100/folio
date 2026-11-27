# Writes trace.png, the oscilloscope trace of the acceptance exam (#57):
# python3 trace.py trace.png. 10 x 8 divisions of 40 px, and a sine of
# period 4 divisions and amplitude 2.5 divisions. Standard library only.
import math, struct, sys, zlib
W, H, D = 400, 320, 40
bg, grid, axis, trace = (18, 28, 24), (62, 84, 74), (110, 140, 125), (90, 255, 140)
px = [[bg] * W for _ in range(H)]
def blend(c, a, b):
    return tuple(round(c[i] * (1 - b) + a[i] * b) for i in range(3))
for y in range(H):
    for x in range(W):
        if x % D == 0 or y % D == 0 or x == W - 1 or y == H - 1:
            px[y][x] = grid
for x in range(W):
    for t in range(-3, 4):
        if x % 8 == 0 and 0 <= H // 2 + t < H:
            px[H // 2 + t][x] = axis
for y in range(H):
    for t in range(-3, 4):
        if y % 8 == 0:
            px[y][W // 2 + t] = axis
for y in range(H):
    for x in range(W):
        best = 9.0
        for sub in range(-12, 13):
            xs = x + sub / 4
            ys = H / 2 - 2.5 * D * math.sin(2 * math.pi * xs / (4 * D))
            best = min(best, math.hypot(xs - x, ys - y))
        if best < 2.6:
            a = 1.0 if best < 1.2 else (2.6 - best) / 1.4
            px[y][x] = blend(px[y][x], trace, a)
raw = b"".join(b"\0" + bytes(c for p in row for c in p) for row in px)
def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 2, 0, 0, 0)) \
    + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
