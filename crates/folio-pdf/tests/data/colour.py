#!/usr/bin/env python3
"""Writes colour.pdf, the PDF figure whose colours go through `moxcms` and
`hayro-jpeg2000` when Folio converts a PDF figure to SVG (SPEC §11.1, §2.4,
#312). `crates/folio-pdf/tests/colour.rs` converts it and compares a digest
of the SVG with the one recorded there, which must be the same on x86-64
with and without AVX2 and FMA and on aarch64.

The page holds:

- swatches and an 8-bit image in two ICC-based RGB spaces, one whose
  tone curves are single gammas (`curv` with one entry) and one whose
  curves are parametric (`para`, the sRGB form), and in an ICC-based grey
  space;
- swatches in a Lab space;
- swatches and an 8-bit image in DeviceCMYK, which hayro converts through
  its CMYK profile, a lookup table;
- a JPEG 2000 image coded with the irreversible (9/7) wavelet and the
  irreversible colour transform, the float paths of hayro-jpeg2000;
- a JPEG image, which hayro decodes with `zune-jpeg`, whose SIMD paths
  (picked by CPU at run time) compute in integers.

The ICC profiles are written here byte by byte. Colours end as 8-bit
values, which hide most single-bit differences, so the images and the rows
of swatches run through every 8-bit level and many in between, where a
rounding difference between two code paths shows.

Run `python3 colour.py` in this directory to write it again. Everything
but the JPEG 2000 and JPEG images uses only the standard library; those
need Pillow (with OpenJPEG), whose output may vary by version, so the
script keeps `colour.j2k` and `colour.jpg` when they exist, and the PDF
stays the same.
"""

import struct
import zlib
from pathlib import Path

HERE = Path(__file__).parent


def s15(v):
    """An s15Fixed16Number."""
    return struct.pack(">i", round(v * 65536))


def xyz_tag(x, y, z):
    return b"XYZ " + b"\0" * 4 + s15(x) + s15(y) + s15(z)


def curv_gamma(g):
    # One entry: a gamma as a u8Fixed8Number.
    return b"curv" + b"\0" * 4 + struct.pack(">IH", 1, round(g * 256)) + b"\0\0"


def para_srgb():
    # Function type 3: Y = (aX + b)^g for X >= d, Y = cX below.
    params = [2.4, 1 / 1.055, 0.055 / 1.055, 1 / 12.92, 0.04045]
    return b"para" + b"\0" * 4 + struct.pack(">HH", 3, 0) + b"".join(map(s15, params))


def profile(space, tags):
    """An ICC v2.1 display profile with the given data colour space and
    tags, a list of (signature, data); PCS XYZ, D50."""
    header_len = 128
    table_len = 4 + 12 * len(tags)
    offset = header_len + table_len
    table = struct.pack(">I", len(tags))
    data = b""
    for sig, body in tags:
        while (offset + len(data)) % 4:
            data += b"\0"
        table += sig + struct.pack(">II", offset + len(data), len(body))
        data += body
    while len(data) % 4:
        data += b"\0"
    size = header_len + table_len + len(data)
    header = (
        struct.pack(">I", size)
        + b"\0" * 4
        + struct.pack(">I", 0x02100000)
        + b"mntr"
        + space
        + b"XYZ "
        + b"\0" * 12
        + b"acsp"
        + b"\0" * 28
        + s15(0.9642)
        + s15(1.0)
        + s15(0.8249)
    )
    header += b"\0" * (header_len - len(header))
    return header + table + data


D50 = (0.9642, 1.0, 0.8249)

# Adobe RGB (1998) primaries adapted to D50, with three gammas.
RGB_GAMMA = profile(
    b"RGB ",
    [
        (b"wtpt", xyz_tag(*D50)),
        (b"rXYZ", xyz_tag(0.6097, 0.3111, 0.0195)),
        (b"gXYZ", xyz_tag(0.2053, 0.6257, 0.0609)),
        (b"bXYZ", xyz_tag(0.1492, 0.0632, 0.7446)),
        (b"rTRC", curv_gamma(2.2)),
        (b"gTRC", curv_gamma(1.8)),
        (b"bTRC", curv_gamma(2.4)),
    ],
)

# Display P3 primaries adapted to D50, with sRGB's parametric curve.
P3 = (
    (0.5151, 0.2412, -0.0011),
    (0.2920, 0.6922, 0.0419),
    (0.1571, 0.0666, 0.7841),
)
RGB_PARA = profile(
    b"RGB ",
    [
        (b"wtpt", xyz_tag(*D50)),
        (b"rXYZ", xyz_tag(*P3[0])),
        (b"gXYZ", xyz_tag(*P3[1])),
        (b"bXYZ", xyz_tag(*P3[2])),
        (b"rTRC", para_srgb()),
        (b"gTRC", para_srgb()),
        (b"bTRC", para_srgb()),
    ],
)

GRAY = profile(b"GRAY", [(b"wtpt", xyz_tag(*D50)), (b"kTRC", curv_gamma(1.8))])

LAB = "[/Lab << /WhitePoint [0.9505 1 1.089] /Range [-128 127 -128 127] >>]"

# Pseudo-random bytes, the same everywhere (a linear congruential
# generator), so that images hold every level in many combinations.
_state = 12345


def rand_byte():
    global _state
    _state = (_state * 1103515245 + 12345) % (1 << 31)
    return (_state >> 16) & 0xFF


def image_bytes(n):
    return bytes(rand_byte() for _ in range(n))


IMG = 64  # image side in samples

objects = []


def add(body):
    objects.append(body)
    return len(objects)


def stream(dict_entries, data, compress=True):
    if compress:
        data = zlib.compress(data, 9)
        dict_entries += " /Filter /FlateDecode"
    return (
        f"<< {dict_entries} /Length {len(data)} >>\nstream\n".encode() + data + b"\nendstream"
    )


icc_gamma = add(stream("/N 3 /Alternate /DeviceRGB", RGB_GAMMA))
icc_para = add(stream("/N 3 /Alternate /DeviceRGB", RGB_PARA))
icc_gray = add(stream("/N 1 /Alternate /DeviceGray", GRAY))

cs = {
    "CSg": f"[/ICCBased {icc_gamma} 0 R]",
    "CSp": f"[/ICCBased {icc_para} 0 R]",
    "CSk": f"[/ICCBased {icc_gray} 0 R]",
    "CSl": LAB,
}


def image(colour_space, components, data=None, filt=None, raw=None):
    if raw is not None:
        return add(
            f"<< /Type /XObject /Subtype /Image /Width {IMG} /Height {IMG}"
            f" /ColorSpace {colour_space} /BitsPerComponent 8 /Filter /{filt}"
            f" /Length {len(raw)} >>\nstream\n".encode()
            + raw
            + b"\nendstream"
        )
    return add(
        stream(
            f"/Type /XObject /Subtype /Image /Width {IMG} /Height {IMG}"
            f" /ColorSpace {colour_space} /BitsPerComponent 8",
            data if data is not None else image_bytes(IMG * IMG * components),
        )
    )


images = {
    "Ig": image(cs["CSg"], 3),
    "Ip": image(cs["CSp"], 3),
    "Ik": image(cs["CSk"], 1),
    "Ic": image("/DeviceCMYK", 4),
}


def jpeg2000():
    path = HERE / "colour.j2k"
    if not path.exists():
        from PIL import Image

        img = Image.frombytes("RGB", (IMG, IMG), image_bytes(IMG * IMG * 3))
        img.save(path, "JPEG2000", irreversible=True, mct=1, no_jp2=True)
    return path.read_bytes()


images["Ij"] = image("/DeviceRGB", 3, filt="JPXDecode", raw=jpeg2000())


def jpeg():
    path = HERE / "colour.jpg"
    if not path.exists():
        from PIL import Image

        # A smooth image with an edge, chroma subsampled, so that the
        # decoder's inverse DCT, upsampling and colour conversion all work.
        data = bytes(
            v
            for row in range(IMG)
            for col in range(IMG)
            for v in (col * 4, row * 4, 255 if (row // 8 + col // 8) % 2 else 32)
        )
        img = Image.frombytes("RGB", (IMG, IMG), data)
        img.save(path, "JPEG", quality=85, subsampling=2)
    return path.read_bytes()


images["Id"] = image("/DeviceRGB", 3, filt="DCTDecode", raw=jpeg())

# The content: rows of swatches, then the images.
ops = []
SW = 4  # swatch side
y = 0


def swatch_row(space, values):
    global y
    ops.append(f"/{space} cs" if space else "")
    for i, v in enumerate(values):
        x = (i % 64) * SW
        if i and i % 64 == 0:
            y += SW
        ops.append(" ".join(f"{c:.5f}" for c in v) + f" {'sc' if space else 'k'}")
        ops.append(f"{x} {y} {SW} {SW} re f")
    y += SW + 2


def levels(n, k):
    """n colours of k components, stepping each through [0, 1] at its own
    rate so that the swatches meet levels between the 8-bit ones."""
    rates = [1, 7, 31, 127][:k]
    return [[((i * r) % (n + 1)) / n for r in rates] for i in range(n)]


swatch_row("CSg", levels(256, 3))
swatch_row("CSp", levels(256, 3))
swatch_row("CSk", levels(256, 1))
swatch_row(
    "CSl",
    [[l * 100, a * 255 - 128, b * 255 - 128] for l, a, b in levels(256, 3)],
)
swatch_row(None, levels(256, 4))  # DeviceCMYK with `k`

for i, name in enumerate(images):
    ops.append(f"q {IMG} 0 0 {IMG} {i * (IMG + 4)} {y} cm /{name} Do Q")
y += IMG

WIDTH, HEIGHT = 64 * SW, y
content = add(stream("", "\n".join(o for o in ops if o).encode()))

resources = (
    "<< /ColorSpace << "
    + " ".join(f"/{k} {v}" for k, v in cs.items())
    + " >> /XObject << "
    + " ".join(f"/{k} {v} 0 R" for k, v in images.items())
    + " >> >>"
)
page = len(objects) + 1
pages = page + 1
add(
    f"<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 {WIDTH} {HEIGHT}]"
    f" /Resources {resources} /Contents {content} 0 R >>".encode()
)
add(f"<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>".encode())
catalog = add(f"<< /Type /Catalog /Pages {pages} 0 R >>".encode())

out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
offsets = []
for i, body in enumerate(objects, 1):
    offsets.append(len(out))
    out += f"{i} 0 obj\n".encode() + (body if isinstance(body, bytes) else body.encode())
    out += b"\nendobj\n"
xref = len(out)
out += f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode()
for o in offsets:
    out += f"{o:010d} 00000 n \n".encode()
out += (
    f"trailer\n<< /Size {len(objects) + 1} /Root {catalog} 0 R >>\n"
    f"startxref\n{xref}\n%%EOF\n"
).encode()
(HERE / "colour.pdf").write_bytes(bytes(out))
