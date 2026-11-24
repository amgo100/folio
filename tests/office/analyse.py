#!/usr/bin/env python3
"""Reports what Word and PowerPoint made of the fixtures, from what
probe-word.ps1 and probe-powerpoint.ps1 wrote to one output directory.
Standard library only. Usage:

    python tests/office/analyse.py <probe output directory>

For PowerPoint: how each effect of effects.pptx was read, before and after
a save; which branch each equation of math-a14.pptx took; and for each
slide of motion-chain.pptx, how far the ball in the video is from the
physical position. For Word: the list labels and field results Word gave numbering-gaps.docx;
for each column set of columns-balance.docx and columns-end.docx,
the lines per column and the last baseline of each column on the set's
last page; and for baseline-grid.docx, the baseline of every line that starts a
paragraph or is off the 13pt grid, with its distance from the grid.
"""

from __future__ import annotations

import json
import re
import sys
import zipfile
from dataclasses import dataclass
from pathlib import Path
from xml.etree import ElementTree

XPS = "{http://schemas.microsoft.com/xps/2005/06}"
LEFT, TEXT_WIDTH = 72.0, 595.3 - 144.0  # A4 with 1-inch margins, in points


@dataclass
class Run:
    page: int
    x: float
    y: float
    size: float
    text: str


@dataclass
class Line:
    page: int
    x: float
    y: float
    size: float
    text: str


def runs(path: Path) -> list[Run]:
    """Glyph runs with their origin in points. Word's XPS pages scale points
    to XPS units (1/96 in) once, on the outer canvas; origins are in points."""
    out = []
    with zipfile.ZipFile(path) as z:
        pages = sorted(
            (n for n in z.namelist() if re.fullmatch(r"Documents/1/Pages/\d+\.fpage", n)),
            key=lambda n: int(re.search(r"(\d+)\.fpage", n).group(1)),
        )
        for number, name in enumerate(pages, start=1):
            root = ElementTree.fromstring(z.read(name))
            for g in root.iter(f"{XPS}Glyphs"):
                text = g.get("UnicodeString", "")
                if text.startswith("{}"):
                    text = text[2:]
                out.append(
                    Run(number, float(g.get("OriginX")), float(g.get("OriginY")),
                        float(g.get("FontRenderingEmSize")), text)
                )
    return out


def lines(rs: list[Run], columns: int = 1, gap: float = 17.0) -> list[Line]:
    """Groups runs into lines by page, column and baseline."""
    width = (TEXT_WIDTH + gap) / columns
    groups: dict[tuple[int, int, float], list[Run]] = {}
    for r in rs:
        col = min(columns - 1, max(0, int((r.x - LEFT) // width)))
        key = (r.page, col, round(r.y, 1))
        groups.setdefault(key, []).append(r)
    out = []
    for (page, _col, y), members in sorted(groups.items(), key=lambda kv: (kv[0][0], kv[0][2], kv[0][1])):
        members.sort(key=lambda r: r.x)
        text = "".join(m.text for m in members).strip()
        if text:
            out.append(Line(page, members[0].x, y, max(m.size for m in members), text))
    return out


def find(ls: list[Line], marker: str) -> Line:
    for line in ls:
        if re.search(rf"(^|\s){re.escape(marker)}(\s|$)", line.text):
            return line
    raise KeyError(marker)


def columns_report(path: Path, cases: list[tuple[str, str, str | None, int]]) -> None:
    rs = runs(path)
    single = lines(rs)
    print(f"\n{path.name}")
    for name, first, end, n in cases:
        start = find(single, first)
        stop = find(single, end) if end else None
        # Lines of the set: between the set's first line and the line after it.
        def inside(r: Run) -> bool:
            after_start = (r.page, r.y) >= (start.page, start.y - 0.01)
            before_end = stop is None or (r.page, r.y) < (stop.page, stop.y - 0.01)
            return after_start and before_end

        set_lines = lines([r for r in rs if inside(r)], n)
        pages = sorted({line.page for line in set_lines})
        print(f"  {name}: {n} columns, pages {pages[0]}-{pages[-1]}")
        for page in pages:
            per_col = []
            width = (TEXT_WIDTH + 17.0) / n
            for c in range(n):
                col = [
                    line for line in set_lines
                    if line.page == page and min(n - 1, int((line.x - LEFT) // width)) == c
                ]
                last = max((line.y for line in col), default=float("nan"))
                per_col.append(f"col {c + 1}: {len(col):2d} lines, last baseline {last:7.2f}pt")
            print(f"    page {page}: " + "; ".join(per_col))


GRID = 13.0
TOLERANCE = 0.15


def grid_report(path: Path) -> None:
    rs = runs(path)
    print(f"\n{path.name} (grid pitch {GRID}pt)")
    # Lines in the two-column section are grouped per column.
    ls_single = lines(rs)
    ref = find(ls_single, "P1A1")
    print(f"  reference: first body baseline {ref.y:.2f}pt on page {ref.page}")
    cols_start = find(ls_single, "CL1")
    cols_end = find(ls_single, "COLEND")
    for page in sorted({r.page for r in rs}):
        page_runs = [r for r in rs if r.page == page]
        in_cols = [r for r in page_runs
                   if (r.page, r.y) >= (cols_start.page, cols_start.y - 0.01)
                   and (r.page, r.y) <= (cols_end.page, cols_end.y + 0.01)]
        rest = [r for r in page_runs if r not in in_cols]
        ls = sorted(lines(rest) + lines(in_cols, 2), key=lambda line: (line.y, line.x))
        print(f"  page {page}")
        for line in ls:
            off = (line.y - ref.y) % GRID
            if off > GRID / 2:
                off -= GRID
            # XPS origins are rounded to 0.1pt.
            if not re.match(r"[A-Z][A-Z0-9]+\b", line.text) and abs(off) <= TOLERANCE:
                continue  # continuation lines on the grid: not printed
            col = " (right column)" if line.x > LEFT + TEXT_WIDTH / 2 else ""
            mark = "on grid" if abs(off) <= TOLERANCE else "OFF GRID"
            print(f"    {line.y:7.2f}pt  {off:+6.2f}  {line.size:5.2f}pt  {mark:8}  "
                  f"{line.text[:24]}{col}")


def numbering_report(path: Path) -> None:
    data = json.loads(path.read_text(encoding="utf-8-sig"))
    n = data["files"]["numbering-gaps"]
    print(f"\nnumbering-gaps.docx (Word {data['word']})")
    for key in ("labels_as_opened", "labels_after_update"):
        labels = n[key]

        def shown(x: dict) -> str:
            # List paragraphs have a list label; captions show their field
            # results as text ("Figure 1.4 expected 1.4").
            return x["label"] or x["text"].split()[1]

        wrong = [f"{shown(x)!r} for {x['expected']!r} ({x['text'][:20]}...)"
                 for x in labels if shown(x) != x["expected"]]
        print(f"  {key}: {len(labels) - len(wrong)} of {len(labels)} as expected"
              + (": " + "; ".join(wrong) if wrong else ""))
    for key in ("fields_as_opened", "fields_after_update"):
        print(f"  {key}: " + " ".join(f["result"] for f in n[key]))
    for case in ("end-of-previous", "start-of-next"):
        r = n[f"insert_{case}"]
        print(f"  question inserted at the {case.replace('-', ' ')} instance: "
              f"gets {r['inserted']}, question 4 then shows {r['question4']}")


# --- PowerPoint ---------------------------------------------------------------

EFFECT_NAMES = {
    0: "custom path", 1: "appear", 2: "fly", 10: "fade", 22: "wipe", 48: "zoom",
    54: "change fill colour", 56: "change font colour", 59: "grow/shrink",
    60: "change line colour", 63: "bold flash", 65: "bold reveal", 75: "pulse",
}
TRIGGERS = {1: "on click", 2: "with previous", 3: "after previous"}


def effects_report(data: dict) -> None:
    deck = data["files"]["effects"]
    print(f"\neffects.pptx (PowerPoint build {data['powerpoint']})")
    if "open_error" in deck:
        print(f"  could not open: {deck['open_error']}")
        return
    for slide in deck["slides"]:
        print(f"  slide {slide['index']}")
        for e in slide["effects"]:
            kind = EFFECT_NAMES.get(e["type"], f"type {e['type']}")
            role = "exit" if e["exit"] else ""
            par = f", paragraph {e['paragraph']}" if e["paragraph"] else ""
            print(f"    {e['shape'][:14]:14} {kind} {role:4} {TRIGGERS[e['trigger']]}, "
                  f"{e['duration']}s{par}")

    def key(slides: list) -> list:
        return [[(e["shape"], e["type"], e["exit"], e["trigger"], e["paragraph"])
                 for e in s["effects"]] for s in slides]

    same = key(deck["slides"]) == key(deck["roundtrip_slides"])
    print(f"  saved again by PowerPoint and reopened: effects {'unchanged' if same else 'CHANGED'}")


def math_report(data: dict, out: Path) -> None:
    deck = data["files"]["math-a14"]
    print("\nmath-a14.pptx")
    copy = out / "math-a14.roundtrip.pptx"
    for slide in deck["slides"]:
        print(f"  slide {slide['index']}")
        for shape in slide["shapes"]:
            if shape["name"].startswith(("Equation", "Inline")):
                # msoFillPicture = 6: the fallback picture; otherwise the a14:m choice.
                branch = "fallback picture" if shape["fill"] == 6 else "a14:m (OMML)"
                print(f"    {shape['name']:16} {branch:18} text {shape['text'].strip()!r}")
        if copy.exists():
            with zipfile.ZipFile(copy) as z:
                xml = z.read(f"ppt/slides/slide{slide['index']}.xml").decode("utf-8")
            print(f"    saved again by PowerPoint: {len(re.findall('<a14:m[ >]', xml))} a14:m "
                  f"elements, {xml.count('<mc:Fallback')} fallbacks")


def read_bmp(path: Path) -> tuple[int, int, int, bytes]:
    data = path.read_bytes()
    offset = int.from_bytes(data[10:14], "little")
    width = int.from_bytes(data[18:22], "little", signed=True)
    height = int.from_bytes(data[22:26], "little", signed=True)
    bpp = int.from_bytes(data[28:30], "little")
    return width, height, bpp, data[offset:]


def ball_centre(path: Path) -> tuple[float, float] | None:
    """Centre of the red ball in a frame, in slide points (frames are 960 px
    wide for a 960pt slide)."""
    width, height, bpp, px = read_bmp(path)
    step = bpp // 8
    stride = (width * step + 3) // 4 * 4
    sx = sy = n = 0
    for row in range(abs(height)):
        y = abs(height) - 1 - row if height > 0 else row
        base = row * stride
        line = px[base:base + width * step]
        for x in range(0, width * step, step):
            b, g, r = line[x], line[x + 1], line[x + 2]
            if r > 180 and g < 90 and b < 90:
                sx += x // step
                sy += y
                n += 1
    if n < 50:
        return None
    scale = 960 / width
    return sx / n * scale, sy / n * scale


def motion_report(out: Path) -> None:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import generate as g

    frames = sorted((out / "motion-frames").glob("*.bmp"))
    if not frames:
        return
    print("\nmotion-chain.pptx (video frames every 100 ms; each slide lasts 6 s)")
    w_pt, h_pt = g.SLIDE_W / g.EMU_PER_PT, g.SLIDE_H / g.EMU_PER_PT

    def to_pt(p: tuple[float, float]) -> tuple[float, float]:
        return g.START_X + p[0] * w_pt, g.START_Y + p[1] * h_pt

    def physical(tau: float) -> tuple[float, float]:
        t = min(max(tau, 0.0), g.PLAY) * g.T_FLIGHT / g.PLAY
        return to_pt(g.to_slide(*g.projectile(t)))

    def bez(ps: list, u: float) -> tuple[float, float]:
        a = (1 - u) ** 3, 3 * u * (1 - u) ** 2, 3 * u * u * (1 - u), u ** 3
        return (sum(k * p[0] for k, p in zip(a, ps)), sum(k * p[1] for k, p in zip(a, ps)))

    def model(n: int, paced: str | None):
        """Position at playback time tau when n chained segments of equal
        duration each move along their Bézier: at uniform speed along the
        curve, measured in points ("pt") or in path units, which are
        fractions of the slide's width and height ("units"); or uniformly in
        the Bézier parameter (None)."""
        segs = []
        for k in range(n):
            raw = g.bezier_points(g.T_FLIGHT * k / n, g.T_FLIGHT * (k + 1) / n)
            pts = [to_pt(bez(raw, i / 400)) for i in range(401)]
            metric = pts if paced == "pt" else [bez(raw, i / 400) for i in range(401)]
            cum = [0.0]
            for a, b in zip(metric, metric[1:]):
                cum.append(cum[-1] + ((b[0] - a[0]) ** 2 + (b[1] - a[1]) ** 2) ** 0.5)
            segs.append((pts, cum))

        def at(tau: float) -> tuple[float, float]:
            f = min(max(tau, 0.0), g.PLAY) / g.PLAY * n
            k = min(int(f), n - 1)
            frac = f - k
            pts, cum = segs[k]
            if paced is None:
                return pts[round(frac * 400)]
            target = frac * cum[-1]
            i = next((i for i, c in enumerate(cum) if c >= target), 400)
            return pts[i]

        return at

    def dist(a: tuple[float, float], b: tuple[float, float]) -> float:
        return ((a[0] - b[0]) ** 2 + (a[1] - b[1]) ** 2) ** 0.5

    positions = {int(f.stem): ball_centre(f) for f in frames}
    slides = [("1 path", 1), ("8 segments, relative to launch point", 8),
              ("8 segments, relative to segment start", 8), ("4 segments", 4),
              ("16 segments", 16)]
    for k, (title, n) in enumerate(slides):
        start = 6000 * k
        # The first frame of a slide can still show the previous one.
        samples = [(ms - start, positions[ms]) for ms in sorted(positions)
                   if start < ms < start + 6000 and positions[ms] is not None]
        if len(samples) < 10:
            print(f"  slide {k + 1}, {title}: ball found in only {len(samples)} frames")
            continue
        units = model(n, "units")
        # Fit the moment the animation starts (frames are 100 ms apart).
        lag = min(range(-100, 201, 5),
                  key=lambda lag: sum(dist(p, units((ms - lag) / 1000)) for ms, p in samples))
        models = (("physical", physical), ("uniform speed in path units", units),
                  ("uniform speed in points", model(n, "pt")))
        err = {name: [dist(p, f((ms - lag) / 1000)) for ms, p in samples
                      if 0 <= (ms - lag) / 1000 <= g.PLAY]
               for name, f in models}
        end = samples[-1][1]
        land = physical(g.PLAY)
        print(f"  slide {k + 1}, {title}: starts {lag} ms into the slide, rests "
              f"{dist(end, land):.0f}pt from the landing point; distance (max, mean) from")
        for name, e in err.items():
            if e:
                print(f"      {name:28} {max(e):6.1f}pt {sum(e) / len(e):6.1f}pt")


def main() -> None:
    out = Path(sys.argv[1])
    if (out / "powerpoint.json").exists():
        data = json.loads((out / "powerpoint.json").read_text(encoding="utf-8-sig"))
        effects_report(data)
        math_report(data, out)
        motion_report(out)
    if not (out / "word.json").exists():
        return
    numbering_report(out / "word.json")
    columns_report(
        out / "columns-balance.xps",
        [
            ("set A (mid-page)", "A01", "ENDA", 2),
            ("set AW (set A, widow control off)", "AW01", "ENDAW", 2),
            ("set B (across a page break)", "B01", "ENDB", 2),
            ("set C (three columns, rule)", "C01", "ENDC", 3),
            ("set D (end of document, empty continuous section after)", "D01", None, 2),
        ],
    )
    columns_report(
        out / "columns-end.xps",
        [("set E (end of document, no section after)", "E01", None, 2)],
    )
    grid_report(out / "baseline-grid.xps")


if __name__ == "__main__":
    main()
