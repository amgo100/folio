#!/usr/bin/env python3
"""Writes spectrum.pdf, the PDF figure of the figures corpus whose colours
go through the maths that #304 made independent of the platform.

The page holds what `hayro-interpret` computes with transcendental float
functions when Folio converts a PDF figure to SVG (SPEC §11.1, §2.4):

- swatches in CalRGB and CalGray, whose gamma is a power;
- a swatch in Lab, decoded with a cube;
- a spot colour (Separation) whose tint transform is a PostScript
  calculator function (Type 4) that uses sin, cos, atan, exp, ln and log;
- an axial shading of that spot colour through an exponential function
  (Type 2) with a fractional exponent, and an axial shading in CalRGB whose
  function is itself a calculator function.

Colours end as 8-bit values, which hide a difference in the last bit of a
float function almost always. So the spot colour is a test pattern: each
channel is the fractional part of a million times a sum of those
functions, which turns a one-unit difference in the last place into a
visible one. Its swatches and its shading, sampled at hundreds of points,
give a PDF whose bytes change if any of these functions gives other bits,
as the C libraries of x86-64 and aarch64 Linux do for the atan
channel.

Only the standard library is used, and the output depends on nothing but
this file: run `python3 spectrum.py` in this directory to write it again.
"""

from pathlib import Path

WIDTH, HEIGHT = 240, 120

# Colour spaces.
CAL_RGB = (
    "[/CalRGB << /WhitePoint [0.9505 1 1.089] /Gamma [2.2 1.8 2.4]"
    " /Matrix [0.4124 0.2126 0.0193 0.3576 0.7152 0.1192 0.1805 0.0722 0.9505] >>]"
)
CAL_GRAY = "[/CalGray << /WhitePoint [0.9505 1 1.089] /Gamma 1.8 >>]"
LAB = "[/Lab << /WhitePoint [0.9505 1 1.089] /Range [-100 100 -100 100] >>]"

# The spot colour's tint transform: tint t in [0, 1] to an RGB colour, where
# frac(x) = |x mod 1|:
#   r = frac(10^4 atan(5 t, 12))
#   g = frac(10^5 (sin(360 t) + cos(180 t)))
#   b = frac(10^5 (log(1 + 9 t) + ln(1 + t) + t ^ 0.6))
# Each product stays below 2^23, so that f32 keeps bits below its units. At
# tints 1 and 0.4921875 (the two swatches), r differs between the C
# libraries of x86-64 and aarch64 Linux (#300, #304).
TINT = (
    b"{ dup 5 mul 12 atan 10000 mul 1 mod abs"
    b" exch dup 360 mul sin"
    b" 1 index 180 mul cos add 100000 mul 1 mod abs"
    b" exch dup 9 mul 1 add log"
    b" 1 index 1 add ln add"
    b" exch 0.6 exp add 100000 mul 1 mod abs }"
)

# The CalRGB shading's function: s in [0, 1] to a CalRGB colour.
RAMP = (
    b"{ dup 0.6 exp"
    b" exch dup 90 mul sin"
    b" exch 2 exp 1 exch sub }"
)

CONTENT = f"""\
q
/CalRGB cs 0.85 0.35 0.15 sc 10 70 40 40 re f
/CalRGB cs 0.2 0.55 0.8 sc 55 70 40 40 re f
/CalGray cs 0.45 sc 100 70 40 40 re f
/Lab cs 62 38 -42 sc 145 70 40 40 re f
/Lab cs 35 -20 30 sc 190 70 40 40 re f
/Spot cs 0.4921875 scn 10 40 40 20 re f
/Spot cs 1 scn 55 40 40 20 re f
q 100 40 130 20 re W n /SpotRamp sh Q
q 10 10 220 20 re W n /RgbRamp sh Q
Q
""".encode("ascii")


def objects():
    """The objects of the file, numbered from 1."""
    return [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        (
            f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {WIDTH} {HEIGHT}]"
            " /Resources << /ColorSpace << /CalRGB 5 0 R /CalGray 6 0 R /Lab 7 0 R"
            " /Spot 8 0 R >> /Shading << /SpotRamp 10 0 R /RgbRamp 12 0 R >> >>"
            " /Contents 4 0 R >>"
        ).encode("ascii"),
        stream(b"", CONTENT),
        CAL_RGB.encode("ascii"),
        CAL_GRAY.encode("ascii"),
        LAB.encode("ascii"),
        b"[/Separation /Spectrum /DeviceRGB 9 0 R]",
        stream(
            b"/FunctionType 4 /Domain [0 1] /Range [0 1 0 1 0 1]",
            TINT,
        ),
        (
            b"<< /ShadingType 2 /ColorSpace 8 0 R /Coords [100 0 230 0]"
            b" /Function 11 0 R /Extend [true true] >>"
        ),
        b"<< /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1.7 >>",
        (
            b"<< /ShadingType 2 /ColorSpace 5 0 R /Coords [10 0 230 0]"
            b" /Function 13 0 R /Extend [true true] >>"
        ),
        stream(
            b"/FunctionType 4 /Domain [0 1] /Range [0 1 0 1 0 1]",
            RAMP,
        ),
    ]


def stream(entries, data):
    return (
        b"<< " + entries + b" /Length " + str(len(data)).encode("ascii") + b" >>\nstream\n"
        + data + b"\nendstream"
    )


def pdf():
    out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = []
    objs = objects()
    for number, body in enumerate(objs, start=1):
        offsets.append(len(out))
        out += f"{number} 0 obj\n".encode("ascii") + body + b"\nendobj\n"
    xref = len(out)
    out += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode("ascii")
    for offset in offsets:
        out += f"{offset:010d} 00000 n \n".encode("ascii")
    out += (
        f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    ).encode("ascii")
    return bytes(out)


if __name__ == "__main__":
    Path(__file__).with_name("spectrum.pdf").write_bytes(pdf())
