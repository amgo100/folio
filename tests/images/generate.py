"""Writes the images of the `figures` corpus project (tests/corpus/figures/assets).

One small image per format Folio reads (SPEC §11.1): PNG, JPEG, WebP and GIF
drawn with Pillow, and an SVG and a one-page PDF written by hand, both vector
only, without text. Run it from anywhere; the output is the same on every run
of the same Pillow version, and the committed files are what the tests use.

    python tests/images/generate.py
"""

import math
import zlib
from pathlib import Path

from PIL import Image, ImageDraw

OUT = Path(__file__).resolve().parent.parent / "corpus" / "figures" / "assets"


def wave_png():
    # A displacement–distance graph: axes and one and a half wavelengths.
    img = Image.new("RGB", (240, 120), "white")
    d = ImageDraw.Draw(img)
    d.line([(10, 60), (230, 60)], fill="black", width=1)
    d.line([(10, 10), (10, 110)], fill="black", width=1)
    points = [(10 + x, 60 - 40 * math.sin(x / 220 * 3 * math.pi)) for x in range(221)]
    d.line(points, fill=(30, 90, 200), width=2)
    img.save(OUT / "wave.png", optimize=False)


def photo_jpg():
    # A smooth gradient with a disc, as a photograph has.
    img = Image.new("RGB", (160, 120))
    px = img.load()
    for y in range(120):
        for x in range(160):
            px[x, y] = (60 + x, 80 + y, 160)
    ImageDraw.Draw(img).ellipse((50, 30, 110, 90), fill=(240, 200, 60))
    img.save(OUT / "photo.jpg", quality=80, optimize=False, progressive=False)


def marker_webp():
    # A disc on a transparent background, lossless.
    img = Image.new("RGBA", (64, 64), (0, 0, 0, 0))
    ImageDraw.Draw(img).ellipse((4, 4, 60, 60), fill=(200, 40, 40, 255))
    img.save(OUT / "marker.webp", lossless=True)


def spring_gif():
    # A zigzag on a palette.
    img = Image.new("P", (96, 48), 0)
    img.putpalette([255, 255, 255, 0, 120, 60] + [0] * (254 * 3))
    d = ImageDraw.Draw(img)
    points = [(4 + 8 * i, 8 if i % 2 else 40) for i in range(12)]
    d.line(points, fill=1, width=3)
    img.save(OUT / "spring.gif")


SVG = """<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="240" height="120" viewBox="0 0 240 120">
  <rect x="0" y="0" width="240" height="120" fill="#ffffff"/>
  <path d="M 20 60 H 70 M 110 60 H 160 M 200 60 H 220 V 100 H 20 V 60"
        fill="none" stroke="#000000" stroke-width="2"/>
  <rect x="70" y="50" width="40" height="20" fill="none" stroke="#000000" stroke-width="2"/>
  <circle cx="180" cy="60" r="20" fill="#f0d070" stroke="#000000" stroke-width="2"/>
  <path d="M 166 46 L 194 74 M 194 46 L 166 74" stroke="#000000" stroke-width="2"/>
  <path d="M 120 20 L 120 40 M 112 28 L 128 28" stroke="#c02020" stroke-width="3"/>
</svg>
"""


def circuit_svg():
    (OUT / "circuit.svg").write_text(SVG, encoding="utf-8", newline="\n")


def field_pdf():
    # One page, 200 × 100 pt: the field lines between two charges, as
    # stroked curves and two filled discs; no fonts.
    k = 0.5523
    def disc(cx, cy, r):
        return (
            f"{cx + r} {cy} m "
            f"{cx + r} {cy + k * r} {cx + k * r} {cy + r} {cx} {cy + r} c "
            f"{cx - k * r} {cy + r} {cx - r} {cy + k * r} {cx - r} {cy} c "
            f"{cx - r} {cy - k * r} {cx - k * r} {cy - r} {cx} {cy - r} c "
            f"{cx + k * r} {cy - r} {cx + r} {cy - k * r} {cx + r} {cy} c f\n"
        )
    content = "0.2 0.3 0.8 RG 1.5 w\n"
    for lift in (-40, -20, 0, 20, 40):
        content += f"50 50 m 90 {50 + lift} 110 {50 + lift} 150 50 c S\n"
    content += "0.8 0.1 0.1 rg\n" + disc(50, 50, 10)
    content += "0.1 0.1 0.8 rg\n" + disc(150, 50, 10)
    stream = zlib.compress(content.encode("ascii"), 9)
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << >> >>",
        b"<< /Length %d /Filter /FlateDecode >>\nstream\n" % len(stream) + stream + b"\nendstream",
    ]
    out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = []
    for i, body in enumerate(objects, 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % i + body + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    for offset in offsets:
        out += b"%010d 00000 n \n" % offset
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objects) + 1, xref)
    (OUT / "field.pdf").write_bytes(bytes(out))


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    wave_png()
    photo_jpg()
    marker_webp()
    spring_gif()
    circuit_svg()
    field_pdf()
