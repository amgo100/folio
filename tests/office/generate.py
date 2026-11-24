#!/usr/bin/env python3
"""Writes the hand-made DOCX and PPTX fixtures that test the Office
assumptions of SPEC §16.2 (issue #37).

Standard library only. The zip entries have fixed timestamps and order, so
running the script again gives byte-identical files. Usage:

    python tests/office/generate.py

The files land next to this script, in `docx/` and `pptx/`. What each file
tests, and what was observed, is in `README.md`.
"""

from __future__ import annotations

import math
import zipfile
from pathlib import Path
from xml.sax.saxutils import escape

HERE = Path(__file__).resolve().parent

# ---------------------------------------------------------------------------
# Packaging


def write_package(path: Path, parts: dict[str, str | bytes]) -> None:
    """Writes an OPC package with fixed timestamps, in the given part order."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in parts.items():
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            if isinstance(data, str):
                data = data.encode("utf-8")
            z.writestr(info, data)


XML_DECL = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
PKG_RELS = "http://schemas.openxmlformats.org/package/2006/relationships"
DOC_RELS = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"


def rels(entries: list[tuple[str, str, str]]) -> str:
    body = "".join(
        f'<Relationship Id="{rid}" Type="{DOC_RELS}/{kind}" Target="{target}"/>'
        for rid, kind, target in entries
    )
    return f'{XML_DECL}<Relationships xmlns="{PKG_RELS}">{body}</Relationships>'


def content_types(defaults: dict[str, str], overrides: dict[str, str]) -> str:
    d = "".join(f'<Default Extension="{e}" ContentType="{t}"/>' for e, t in defaults.items())
    o = "".join(f'<Override PartName="{p}" ContentType="{t}"/>' for p, t in overrides.items())
    return (
        f"{XML_DECL}<Types xmlns="
        f'"http://schemas.openxmlformats.org/package/2006/content-types">{d}{o}</Types>'
    )


# ---------------------------------------------------------------------------
# DOCX

W_NS = (
    'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" '
    f'xmlns:r="{DOC_RELS}"'
)
WML = "application/vnd.openxmlformats-officedocument.wordprocessingml"

# A4 with 1-inch margins, in twips.
PAGE_W, PAGE_H, MARGIN = 11906, 16838, 1440

WORDS = (
    "the ball leaves the ground at an angle and follows a parabola until it lands "
    "while its horizontal velocity stays constant and its vertical velocity changes "
    "at a steady rate because the only force acting on it is its weight"
).split()


def filler(seed: int, n: int) -> str:
    """Deterministic filler text of n words."""
    return " ".join(WORDS[(seed * 7 + i) % len(WORDS)] for i in range(n))


def run(text: str, rpr: str = "") -> str:
    rp = f"<w:rPr>{rpr}</w:rPr>" if rpr else ""
    return f'<w:r>{rp}<w:t xml:space="preserve">{escape(text)}</w:t></w:r>'


def field(instr: str, cached: str, rpr: str = "") -> str:
    rp = f"<w:rPr>{rpr}</w:rPr>" if rpr else ""
    return (
        f'<w:r>{rp}<w:fldChar w:fldCharType="begin"/></w:r>'
        f'<w:r>{rp}<w:instrText xml:space="preserve"> {escape(instr)} </w:instrText></w:r>'
        f'<w:r>{rp}<w:fldChar w:fldCharType="separate"/></w:r>'
        f"{run(cached, rpr)}"
        f'<w:r>{rp}<w:fldChar w:fldCharType="end"/></w:r>'
    )


def para(content: str, ppr: str = "") -> str:
    pp = f"<w:pPr>{ppr}</w:pPr>" if ppr else ""
    return f"<w:p>{pp}{content}</w:p>"


def style_ref(style: str) -> str:
    return f'<w:pStyle w:val="{style}"/>'


def sect_pr(
    *,
    kind: str = "continuous",
    cols: int = 1,
    space: int = 340,
    sep: bool = False,
    grid: str = "",
) -> str:
    sep_attr = ' w:sep="1"' if sep else ""
    return (
        "<w:sectPr>"
        f'<w:type w:val="{kind}"/>'
        f'<w:pgSz w:w="{PAGE_W}" w:h="{PAGE_H}"/>'
        f'<w:pgMar w:top="{MARGIN}" w:right="{MARGIN}" w:bottom="{MARGIN}" '
        f'w:left="{MARGIN}" w:header="709" w:footer="709" w:gutter="0"/>'
        f'<w:cols w:num="{cols}" w:space="{space}"{sep_attr}/>'
        f"{grid}"
        "</w:sectPr>"
    )


def docx_styles(extra: str = "", body_font: str = "Calibri", size: int = 22) -> str:
    return (
        f"{XML_DECL}<w:styles {W_NS}>"
        "<w:docDefaults><w:rPrDefault><w:rPr>"
        f'<w:rFonts w:ascii="{body_font}" w:hAnsi="{body_font}" w:cs="{body_font}" '
        f'w:eastAsia="{body_font}"/>'
        f'<w:sz w:val="{size}"/><w:szCs w:val="{size}"/><w:lang w:val="en-GB"/>'
        "</w:rPr></w:rPrDefault>"
        '<w:pPrDefault><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/>'
        "</w:pPr></w:pPrDefault></w:docDefaults>"
        '<w:style w:type="paragraph" w:default="1" w:styleId="Normal">'
        '<w:name w:val="Normal"/><w:qFormat/></w:style>'
        '<w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont">'
        '<w:name w:val="Default Paragraph Font"/><w:uiPriority w:val="1"/>'
        "<w:semiHidden/></w:style>"
        '<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/>'
        '<w:basedOn w:val="Normal"/><w:pPr><w:spacing w:after="120"/></w:pPr>'
        '<w:rPr><w:b/><w:sz w:val="28"/></w:rPr></w:style>'
        f"{extra}</w:styles>"
    )


SETTINGS = (
    f"{XML_DECL}<w:settings {W_NS}>"
    '<w:defaultTabStop w:val="720"/>'
    '<w:characterSpacingControl w:val="doNotCompress"/>'
    "<w:compat>"
    '<w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" '
    'w:val="15"/>'
    "</w:compat></w:settings>"
)


def docx_package(path: Path, body: str, styles: str, numbering: str | None = None) -> None:
    overrides = {
        "/word/document.xml": f"{WML}.document.main+xml",
        "/word/styles.xml": f"{WML}.styles+xml",
        "/word/settings.xml": f"{WML}.settings+xml",
    }
    doc_rels = [("rId1", "styles", "styles.xml"), ("rId2", "settings", "settings.xml")]
    if numbering is not None:
        overrides["/word/numbering.xml"] = f"{WML}.numbering+xml"
        doc_rels.append(("rId3", "numbering", "numbering.xml"))
    parts: dict[str, str | bytes] = {
        "[Content_Types].xml": content_types(
            {
                "rels": "application/vnd.openxmlformats-package.relationships+xml",
                "xml": "application/xml",
            },
            overrides,
        ),
        "_rels/.rels": rels([("rId1", "officeDocument", "word/document.xml")]),
        "word/_rels/document.xml.rels": rels(doc_rels),
        "word/document.xml": f"{XML_DECL}<w:document {W_NS}><w:body>{body}</w:body></w:document>",
        "word/styles.xml": styles,
        "word/settings.xml": SETTINGS,
    }
    if numbering is not None:
        parts["word/numbering.xml"] = numbering
    write_package(path, parts)


# --- §13.2: column balancing before a continuous section break -------------


def column_set(tag: str, paragraphs: int, words: int, sect: str, ppr: str = "") -> str:
    """Paragraphs of a column set; the last one carries the set's sectPr."""
    out = []
    for i in range(1, paragraphs + 1):
        text = f"{tag}{i:02d} {filler(i + len(tag[:1]), words + (i * 5) % 11)}"
        out.append(para(run(text), ppr + (sect if i == paragraphs else "")))
    return "".join(out)


def columns_balance() -> None:
    """Five column sets: mid-page (with Word's default widow control, and
    again with it off), across a page break, three columns with a rule, and
    one at the end of the document followed by an empty continuous section.
    Each set is closed by a continuous section break."""
    title = style_ref("Title")
    single_first = sect_pr(kind="nextPage")
    single = sect_pr()
    body = "".join(
        [
            para(run("Column balancing before continuous section breaks (SPEC §13.2)"), title),
            para(run("CASEA Two columns, mid-page, 9 paragraphs. " + filler(1, 30))),
            para(run("Single-column text before the set."), single_first),
            column_set("A", 9, 18, sect_pr(cols=2)),
            para(run("ENDA After set A. " + filler(2, 20))),
            para(run("CASEW Set A again with widow control off."), single),
            column_set("AW", 9, 18, sect_pr(cols=2), '<w:widowControl w:val="0"/>'),
            para(run("ENDAW After set AW.")),
            para(run("CASEB Two columns running across a page break. " + filler(3, 20)), single),
            column_set("B", 40, 40, sect_pr(cols=2)),
            para(run("ENDB After set B. " + filler(4, 20))),
            para(run("CASEC Three columns with a rule, mid-page. " + filler(5, 20)), single),
            column_set("C", 10, 16, sect_pr(cols=3, sep=True)),
            para(run("ENDC After set C. " + filler(6, 20))),
            para(
                run(
                    "CASED Two columns at the end of the document, followed by an "
                    "empty continuous section."
                ),
                single,
            ),
            column_set("D", 7, 18, sect_pr(cols=2)),
            para(""),
            sect_pr(),
        ]
    )
    docx_package(HERE / "docx" / "columns-balance.docx", body, docx_styles())

    # Control: the same set at the end of the document with no section after it.
    body = "".join(
        [
            para(run("Column set at the end of the document, no section after it"), title),
            para(run("CASEE " + filler(1, 30)), sect_pr(kind="nextPage")),
            column_set("E", 7, 18, ""),
            sect_pr(cols=2),
        ]
    )
    docx_package(HERE / "docx" / "columns-end.docx", body, docx_styles())


# --- §7.3: baseline grid ----------------------------------------------------

PITCH = 260  # 13pt in twips


def grid_styles() -> str:
    def pstyle(sid: str, name: str, ppr: str, rpr: str) -> str:
        return (
            f'<w:style w:type="paragraph" w:customStyle="1" w:styleId="{sid}">'
            f'<w:name w:val="{name}"/><w:basedOn w:val="Normal"/>'
            f"<w:pPr>{ppr}</w:pPr><w:rPr>{rpr}</w:rPr></w:style>"
        )

    exact = f'<w:spacing w:before="0" w:after="0" w:line="{PITCH}" w:lineRule="exact"/>'
    auto = '<w:spacing w:before="0" w:after="0" w:line="240" w:lineRule="auto"/>'
    return "".join(
        [
            # Exact line spacing equal to the grid pitch.
            pstyle("Body", "Body", exact, '<w:sz w:val="20"/>'),
            pstyle("BodySmall", "Body Small", exact, '<w:sz w:val="16"/>'),
            pstyle("BodyLarge", "Body Large", exact, '<w:sz w:val="24"/>'),
            # Two grid lines tall in total (2 x 13pt): on the grid.
            pstyle(
                "HeadInt",
                "Heading Integral",
                f'<w:keepNext/><w:spacing w:before="{PITCH}" w:after="0" '
                f'w:line="{PITCH}" w:lineRule="exact"/>',
                '<w:b/><w:sz w:val="28"/>',
            ),
            # 6pt + 17pt = 23pt: not a whole number of grid lines.
            pstyle(
                "HeadOdd",
                "Heading Odd",
                '<w:keepNext/><w:spacing w:before="120" w:after="0" w:line="340" '
                'w:lineRule="exact"/>',
                '<w:b/><w:sz w:val="28"/>',
            ),
            # 6pt + 17pt + 3pt = 26pt: the writer pads the space after so the
            # block is two grid lines, as Folio's grid-snap: block does.
            pstyle(
                "HeadPad",
                "Heading Padded",
                '<w:keepNext/><w:spacing w:before="120" w:after="60" w:line="340" '
                'w:lineRule="exact"/>',
                '<w:b/><w:sz w:val="28"/>',
            ),
            # Auto (single) spacing, relying on Word's grid to snap line heights.
            pstyle("BodyAuto", "Body Auto", auto, '<w:sz w:val="20"/>'),
            pstyle(
                "HeadAuto",
                "Heading Auto",
                '<w:keepNext/><w:spacing w:before="120" w:after="0" w:line="240" '
                'w:lineRule="auto"/>',
                '<w:b/><w:sz w:val="28"/>',
            ),
        ]
    )


def baseline_grid() -> None:
    """Body text with exact 13pt leading on a 13pt document grid, interrupted
    by blocks that are or are not whole grid lines tall; a two-column
    section; a page that starts with a heading (page break before); then the
    same with auto line spacing (Word's grid snapping) and with exact spacing
    and no grid; and last a heading pushed to the top of a page by a natural
    page break."""
    grid = f'<w:docGrid w:type="lines" w:linePitch="{PITCH}"/>'
    no_grid = f'<w:docGrid w:linePitch="{PITCH}"/>'

    def body(tag: str, n: int, style: str = "Body", words: int = 45) -> str:
        return "".join(
            para(run(f"{tag}{i} {filler(i + len(tag), words)}"), style_ref(style))
            for i in range(1, n + 1)
        )

    def head(tag: str, style: str, extra: str = "") -> str:
        return para(run(f"{tag} heading"), style_ref(style) + extra)

    page_break = '<w:pageBreakBefore/>'
    content = "".join(
        [
            # Page 1: exact spacing, grid on.
            body("P1A", 3),
            head("HINT", "HeadInt"),
            body("P1B", 2),
            head("HPAD", "HeadPad"),
            body("P1C", 2),
            para(run("SMALL " + filler(1, 40)), style_ref("BodySmall")),
            para(run("LARGE " + filler(2, 30)), style_ref("BodyLarge")),
            body("P1D", 2),
            head("HODD", "HeadOdd"),
            body("P1E", 2),
            # Page 2 starts with a heading that has space before.
            head("HTOP", "HeadInt", page_break),
            body("P2A", 2),
            para(run("COLS two-column section follows."), style_ref("Body") + sect_pr(grid=grid)),
            body("CL", 6, words=30),
            para(run("COLEND"), style_ref("Body") + sect_pr(cols=2, grid=grid)),
            body("P2B", 2),
            para(run("END1"), style_ref("Body") + sect_pr(grid=grid)),
            # Page 3: auto spacing, grid on.
            para(run("AUTO section: single line spacing, document grid on."),
                 style_ref("BodyAuto")),
            body("P3A", 3, "BodyAuto"),
            head("HAUTO", "HeadAuto"),
            body("P3B", 3, "BodyAuto"),
            para(run("END2"), style_ref("BodyAuto") + sect_pr(kind="nextPage", grid=grid)),
            # Page 4: exact spacing, no grid.
            para(run("NOGRID section: exact line spacing, no document grid."), style_ref("Body")),
            body("P4A", 3),
            head("HPAD4", "HeadPad"),
            body("P4B", 3),
            para(run("END3"), style_ref("Body") + sect_pr(kind="nextPage", grid=no_grid)),
            # Page 5: 52 one-line paragraphs leave 8.9pt, too little for the
            # 26pt heading, so it starts page 6 at a natural page break.
            "".join(para(run(f"line {i}"), style_ref("Body")) for i in range(1, 53)),
            head("HNAT", "HeadInt"),
            body("P6A", 2),
            sect_pr(kind="nextPage", grid=grid),
        ]
    )
    docx_package(HERE / "docx" / "baseline-grid.docx", content, docx_styles(grid_styles()))


# --- §11.2: gapped numbering ------------------------------------------------


def lvl(ilvl: int, fmt: str, text: str, indent: int, pstyle: str = "") -> str:
    ps = f'<w:pStyle w:val="{pstyle}"/>' if pstyle else ""
    return (
        f'<w:lvl w:ilvl="{ilvl}"><w:start w:val="1"/><w:numFmt w:val="{fmt}"/>{ps}'
        f'<w:lvlText w:val="{text}"/><w:lvlJc w:val="left"/>'
        f'<w:pPr><w:ind w:left="{indent}" w:hanging="425"/></w:pPr></w:lvl>'
    )


def question_abstract(aid: int, name: str) -> str:
    return (
        f'<w:abstractNum w:abstractNumId="{aid}"><w:multiLevelType w:val="multilevel"/>'
        f'<w:name w:val="{name}"/>'
        + lvl(0, "decimal", "%1.", 425)
        + lvl(1, "lowerLetter", "(%2)", 850)
        + lvl(2, "lowerRoman", "(%3)", 1275)
        + "</w:abstractNum>"
    )


def num(nid: int, aid: int, starts: dict[int, int] | None = None) -> str:
    ov = "".join(
        f'<w:lvlOverride w:ilvl="{i}"><w:startOverride w:val="{v}"/></w:lvlOverride>'
        for i, v in (starts or {}).items()
    )
    return f'<w:num w:numId="{nid}"><w:abstractNumId w:val="{aid}"/>{ov}</w:num>'


def numbering_gaps() -> None:
    """An exam whose numbering group leaves gaps (questions and parts that only
    the mark scheme shows), as the DOCX writer of §11.2 would export it:
    a new list instance with start overrides at each gap, chapter-numbered
    figure captions with `SEQ` resets, and a gapped chapter number.

    Three encodings of a gap are tested side by side:
      list X (abstractNum 0): the new instance overrides only the gapped level;
      list Y (abstractNum 2): the new instance overrides the gapped level and
      every level above it with the values they have at the gap;
      list Z (abstractNum 3): as Y, and a gap in a lower level also ends its
      instance at the next item of a higher level, which starts another
      instance (Question 5 below).
    Each numbered paragraph says which label it expects."""
    numbering = (
        f'{XML_DECL}<w:numbering {W_NS}>'
        + question_abstract(0, "Questions X")
        + (
            '<w:abstractNum w:abstractNumId="1"><w:multiLevelType w:val="multilevel"/>'
            '<w:name w:val="Chapters"/>'
            + lvl(0, "decimal", "%1", 0, "Heading1")
            + "</w:abstractNum>"
        )
        + question_abstract(2, "Questions Y")
        + question_abstract(3, "Questions Z")
        # List X.
        + num(1, 0)
        + num(2, 0, {0: 4})  # Question 3 hidden.
        + num(3, 0, {1: 3})  # Part 4(b) hidden: only level 1 overridden.
        + num(4, 0, {0: 7})  # Question 6 hidden.
        # Chapters.
        + num(10, 1)
        + num(11, 1, {0: 4})  # Chapter 3 hidden.
        # List Y.
        + num(21, 2)
        + num(22, 2, {0: 4})
        + num(23, 2, {0: 4, 1: 3})  # Part 4(b) hidden: level 0 kept at 4.
        + num(24, 2, {0: 7})
        # List Z.
        + num(31, 3)
        + num(32, 3, {0: 4})
        + num(33, 3, {0: 4, 1: 3})
        + num(34, 3, {0: 5})  # Question 5 ends the instance of part 4(c).
        + num(35, 3, {0: 7})
        + "</w:numbering>"
    )
    styles = docx_styles(
        '<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/>'
        '<w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/>'
        '<w:pPr><w:keepNext/><w:numPr><w:numId w:val="10"/></w:numPr>'
        '<w:spacing w:before="240" w:after="120"/><w:outlineLvl w:val="0"/></w:pPr>'
        '<w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style>'
        '<w:style w:type="paragraph" w:customStyle="1" w:styleId="Question">'
        '<w:name w:val="Question"/><w:basedOn w:val="Normal"/>'
        '<w:pPr><w:spacing w:after="60"/></w:pPr></w:style>'
        '<w:style w:type="paragraph" w:styleId="Caption"><w:name w:val="caption"/>'
        '<w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/>'
        '<w:pPr><w:spacing w:after="120"/></w:pPr><w:rPr><w:i/></w:rPr></w:style>'
    )

    def q(numid: int, ilvl: int, expect: str, text: str = "") -> str:
        ppr = (
            style_ref("Question")
            + f'<w:numPr><w:ilvl w:val="{ilvl}"/><w:numId w:val="{numid}"/></w:numPr>'
        )
        return para(run(f"Expected {expect} {text or filler(numid + ilvl, 8)}"), ppr)

    def chapter(title: str, expect: str, numid: int | None = None) -> str:
        ppr = style_ref("Heading1")
        if numid is not None:
            ppr += f'<w:numPr><w:numId w:val="{numid}"/></w:numPr>'
        return para(run(f"{title}, expected {expect}"), ppr)

    def figure(chap: str, n: str, reset: int | None = None) -> str:
        sw = f" \\r {reset}" if reset is not None else ""
        return para(
            run("Figure ")
            + field("STYLEREF 1 \\s", chap)
            + run(".")
            + field(f"SEQ Figure \\* ARABIC{sw} \\s 1", n)
            + run(f" expected {chap}.{n}"),
            style_ref("Caption"),
        )

    def table(n: str, reset: int | None = None) -> str:
        sw = f" \\r {reset}" if reset is not None else ""
        return para(
            run("Table ") + field(f"SEQ Table \\* ARABIC{sw}", n) + run(f" expected {n}"),
            style_ref("Caption"),
        )

    def questions(base: int, split_after_part_gap: bool = False) -> str:
        # base 0 -> list X ids 1..4, base 20 -> list Y ids 21..24,
        # base 30 -> list Z ids 31..35.
        a, b, c = base + 1, base + 2, base + 3
        c2 = base + 4 if split_after_part_gap else c
        d = base + 5 if split_after_part_gap else base + 4
        return "".join(
            [
                q(a, 0, "1."),
                q(a, 1, "(a)"),
                q(a, 1, "(b)"),
                q(a, 0, "2.", "INSERTAFTER " + filler(2, 6)),
                q(a, 1, "(a)"),
                # Question 3 is mark-scheme only.
                q(b, 0, "4."),
                q(b, 1, "(a)"),
                # Part 4(b) is mark-scheme only.
                q(c, 1, "(c)"),
                q(c2, 0, "5."),
                q(c2, 1, "(a)"),
                # Question 6 is mark-scheme only.
                q(d, 0, "7."),
                q(d, 1, "(a)"),
            ]
        )

    body = "".join(
        [
            para(run("Gapped numbering at export (SPEC §11.2)"), style_ref("Title")),
            chapter("Kinematics", "1"),
            para(run("List X: new instances override only the gapped level.")),
            questions(0),
            para(run("List Y: new instances override the gapped level and the levels above.")),
            questions(20),
            para(run("List Z: as Y, and the next question after a part gap starts an instance.")),
            questions(30, split_after_part_gap=True),
            figure("1", "1"),
            figure("1", "2"),
            figure("1", "4", reset=4),  # Figure 1.3 is mark-scheme only.
            table("1"),
            table("3", reset=3),  # Table 2 is mark-scheme only.
            chapter("Dynamics", "2"),
            figure("2", "1"),
            figure("2", "2"),
            table("4"),
            # Chapter 3 is mark-scheme only.
            chapter("Energy", "4", numid=11),
            figure("4", "1"),
            figure("4", "3", reset=3),  # Figure 4.2 is mark-scheme only.
            sect_pr(kind="nextPage"),
        ]
    )
    docx_package(HERE / "docx" / "numbering-gaps.docx", body, styles, numbering)


# ---------------------------------------------------------------------------
# PPTX

P_NS = (
    'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" '
    f'xmlns:r="{DOC_RELS}" '
    'xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"'
)
PML = "application/vnd.openxmlformats-officedocument.presentationml"
SLIDE_W, SLIDE_H = 12192000, 6858000  # 16:9
EMU_PER_PT = 12700

THEME = (
    f'{XML_DECL}<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" '
    'name="Folio test"><a:themeElements>'
    '<a:clrScheme name="Folio test">'
    '<a:dk1><a:srgbClr val="000000"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1>'
    '<a:dk2><a:srgbClr val="1F2937"/></a:dk2><a:lt2><a:srgbClr val="E5E7EB"/></a:lt2>'
    '<a:accent1><a:srgbClr val="2563EB"/></a:accent1>'
    '<a:accent2><a:srgbClr val="DC2626"/></a:accent2>'
    '<a:accent3><a:srgbClr val="16A34A"/></a:accent3>'
    '<a:accent4><a:srgbClr val="CA8A04"/></a:accent4>'
    '<a:accent5><a:srgbClr val="9333EA"/></a:accent5>'
    '<a:accent6><a:srgbClr val="0891B2"/></a:accent6>'
    '<a:hlink><a:srgbClr val="2563EB"/></a:hlink>'
    '<a:folHlink><a:srgbClr val="9333EA"/></a:folHlink></a:clrScheme>'
    '<a:fontScheme name="Folio test">'
    '<a:majorFont><a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont>'
    '<a:minorFont><a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont>'
    "</a:fontScheme>"
    '<a:fmtScheme name="Folio test"><a:fillStyleLst>'
    '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
    '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
    '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
    "</a:fillStyleLst><a:lnStyleLst>"
    '<a:ln w="6350"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln>'
    '<a:ln w="12700"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln>'
    '<a:ln w="19050"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln>'
    "</a:lnStyleLst><a:effectStyleLst>"
    "<a:effectStyle><a:effectLst/></a:effectStyle>"
    "<a:effectStyle><a:effectLst/></a:effectStyle>"
    "<a:effectStyle><a:effectLst/></a:effectStyle>"
    "</a:effectStyleLst><a:bgFillStyleLst>"
    '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
    '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
    '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
    "</a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>"
)

EMPTY_TREE = (
    '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>'
    '<p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/>'
    '<a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>'
)

MASTER = (
    f"{XML_DECL}<p:sldMaster {P_NS}><p:cSld>"
    '<p:bg><p:bgRef idx="1001"><a:schemeClr val="bg1"/></p:bgRef></p:bg>'
    f"<p:spTree>{EMPTY_TREE}</p:spTree></p:cSld>"
    '<p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" '
    'accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" '
    'hlink="hlink" folHlink="folHlink"/>'
    '<p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst>'
    "</p:sldMaster>"
)

LAYOUT = (
    f'{XML_DECL}<p:sldLayout {P_NS} type="blank" preserve="1">'
    f'<p:cSld name="Blank"><p:spTree>{EMPTY_TREE}</p:spTree></p:cSld>'
    "<p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"
)


def pptx_package(path: Path, slides: list[tuple[str, list[tuple[str, str, bytes]]]]) -> None:
    """slides: (slide XML, [(rId, target name under ppt/media, bytes)])."""
    overrides = {
        "/ppt/presentation.xml": f"{PML}.presentation.main+xml",
        "/ppt/slideMasters/slideMaster1.xml": f"{PML}.slideMaster+xml",
        "/ppt/slideLayouts/slideLayout1.xml": f"{PML}.slideLayout+xml",
        "/ppt/theme/theme1.xml": "application/vnd.openxmlformats-officedocument.theme+xml",
    }
    for i in range(1, len(slides) + 1):
        overrides[f"/ppt/slides/slide{i}.xml"] = f"{PML}.slide+xml"
    sld_ids = "".join(
        f'<p:sldId id="{255 + i}" r:id="rId{i + 1}"/>' for i in range(1, len(slides) + 1)
    )
    presentation = (
        f'{XML_DECL}<p:presentation {P_NS} saveSubsetFonts="1">'
        '<p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst>'
        f"<p:sldIdLst>{sld_ids}</p:sldIdLst>"
        f'<p:sldSz cx="{SLIDE_W}" cy="{SLIDE_H}"/><p:notesSz cx="6858000" cy="9144000"/>'
        "</p:presentation>"
    )
    pres_rels = [("rId1", "slideMaster", "slideMasters/slideMaster1.xml")]
    pres_rels += [(f"rId{i + 1}", "slide", f"slides/slide{i}.xml") for i in range(1, len(slides) + 1)]
    pres_rels.append((f"rId{len(slides) + 2}", "theme", "theme/theme1.xml"))
    parts: dict[str, str | bytes] = {
        "[Content_Types].xml": content_types(
            {
                "rels": "application/vnd.openxmlformats-package.relationships+xml",
                "xml": "application/xml",
                "png": "image/png",
            },
            overrides,
        ),
        "_rels/.rels": rels([("rId1", "officeDocument", "ppt/presentation.xml")]),
        "ppt/presentation.xml": presentation,
        "ppt/_rels/presentation.xml.rels": rels(pres_rels),
        "ppt/slideMasters/slideMaster1.xml": MASTER,
        "ppt/slideMasters/_rels/slideMaster1.xml.rels": rels(
            [
                ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
                ("rId2", "theme", "../theme/theme1.xml"),
            ]
        ),
        "ppt/slideLayouts/slideLayout1.xml": LAYOUT,
        "ppt/slideLayouts/_rels/slideLayout1.xml.rels": rels(
            [("rId1", "slideMaster", "../slideMasters/slideMaster1.xml")]
        ),
        "ppt/theme/theme1.xml": THEME,
    }
    for i, (xml, media) in enumerate(slides, start=1):
        entries = [("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml")]
        for rid, name, data in media:
            entries.append((rid, "image", f"../media/{name}"))
            parts[f"ppt/media/{name}"] = data
        parts[f"ppt/slides/slide{i}.xml"] = xml
        parts[f"ppt/slides/_rels/slide{i}.xml.rels"] = rels(entries)
    write_package(path, parts)


def emu(pt: float) -> int:
    return round(pt * EMU_PER_PT)


def sp(
    spid: int,
    name: str,
    x: float,
    y: float,
    w: float,
    h: float,
    paragraphs: list[str] | None = None,
    *,
    fill: str | None = "E5E7EB",
    line: str | None = "1F2937",
    line_w: float = 1,
    dash: str | None = None,
    geom: str = "rect",
    size: int = 1800,
    blip: str | None = None,
) -> str:
    """A shape at (x, y, w, h) in points with optional text paragraphs."""
    if blip is not None:
        fill_xml = (
            f'<a:blipFill><a:blip r:embed="{blip}"/><a:stretch><a:fillRect/></a:stretch>'
            "</a:blipFill>"
        )
    elif fill is not None:
        fill_xml = f'<a:solidFill><a:srgbClr val="{fill}"/></a:solidFill>'
    else:
        fill_xml = "<a:noFill/>"
    if line is not None:
        d = f'<a:prstDash val="{dash}"/>' if dash else ""
        line_xml = (
            f'<a:ln w="{emu(line_w)}"><a:solidFill><a:srgbClr val="{line}"/></a:solidFill>{d}</a:ln>'
        )
    else:
        line_xml = "<a:ln><a:noFill/></a:ln>"
    paras = paragraphs if paragraphs is not None else [""]
    body = "".join(
        p if p.startswith("<a:p>") else text_p(p, size) for p in paras
    )
    return (
        f'<p:sp><p:nvSpPr><p:cNvPr id="{spid}" name="{escape(name)}"/><p:cNvSpPr/><p:nvPr/>'
        f"</p:nvSpPr><p:spPr>"
        f'<a:xfrm><a:off x="{emu(x)}" y="{emu(y)}"/><a:ext cx="{emu(w)}" cy="{emu(h)}"/></a:xfrm>'
        f'<a:prstGeom prst="{geom}"><a:avLst/></a:prstGeom>{fill_xml}{line_xml}</p:spPr>'
        f'<p:txBody><a:bodyPr wrap="square" lIns="91440" tIns="45720" rIns="91440" '
        f'bIns="45720" rtlCol="0" anchor="ctr"><a:noAutofit/></a:bodyPr><a:lstStyle/>'
        f"{body}</p:txBody></p:sp>"
    )


def pic(spid: int, name: str, x: float, y: float, w: float, h: float, rid: str) -> str:
    """A picture at (x, y, w, h) in points."""
    return (
        f'<p:pic><p:nvPicPr><p:cNvPr id="{spid}" name="{escape(name)}"/>'
        '<p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr>'
        f'<p:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></p:blipFill>'
        f'<p:spPr><a:xfrm><a:off x="{emu(x)}" y="{emu(y)}"/><a:ext cx="{emu(w)}" cy="{emu(h)}"/>'
        '</a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic>'
    )


def text_p(text: str, size: int = 1800, algn: str = "ctr") -> str:
    if not text:
        return f'<a:p><a:pPr algn="{algn}"/><a:endParaRPr lang="en-GB" sz="{size}"/></a:p>'
    return (
        f'<a:p><a:pPr algn="{algn}"/><a:r><a:rPr lang="en-GB" sz="{size}" dirty="0">'
        f'<a:solidFill><a:srgbClr val="111111"/></a:solidFill></a:rPr>'
        f"<a:t>{escape(text)}</a:t></a:r></a:p>"
    )


def slide_xml(shapes: str, timing: str = "") -> str:
    return (
        f"{XML_DECL}<p:sld {P_NS}><p:cSld><p:spTree>{EMPTY_TREE}{shapes}"
        f"</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr>"
        f"{timing}</p:sld>"
    )


# --- Timing -----------------------------------------------------------------


class Ids:
    def __init__(self) -> None:
        self.n = 2  # 1 is the root, 2 the main sequence

    def __call__(self) -> int:
        self.n += 1
        return self.n


def tgt(spid: int, para_index: int | None) -> str:
    if para_index is None:
        return f'<p:spTgt spid="{spid}"/>'
    return (
        f'<p:spTgt spid="{spid}"><p:txEl><p:pRg st="{para_index}" end="{para_index}"/>'
        "</p:txEl></p:spTgt>"
    )


def bhvr(ids: Ids, target: str, dur: int | str, attrs: list[str] = (), *, extra: str = "",
         ctn_extra: str = "", delay: int | None = None, additive: str = "") -> str:
    st = f'<p:stCondLst><p:cond delay="{delay}"/></p:stCondLst>' if delay is not None else ""
    names = (
        "<p:attrNameLst>" + "".join(f"<p:attrName>{a}</p:attrName>" for a in attrs)
        + "</p:attrNameLst>"
        if attrs
        else ""
    )
    add = f' additive="{additive}"' if additive else ""
    return (
        f'<p:cBhvr{add}{extra}><p:cTn id="{ids()}" dur="{dur}"{ctn_extra}>{st}</p:cTn>'
        f"<p:tgtEl>{target}</p:tgtEl>{names}</p:cBhvr>"
    )


def set_vis(ids: Ids, target: str, value: str, delay: int = 0) -> str:
    return (
        f'<p:set>{bhvr(ids, target, 1, ["style.visibility"], ctn_extra=" fill=\"hold\"", delay=delay)}'
        f'<p:to><p:strVal val="{value}"/></p:to></p:set>'
    )


def anim(ids: Ids, target: str, attr: str, dur: int, frm: str, to: str, additive: str = "") -> str:
    def val(v: str) -> str:
        try:
            float(v)
            return f'<p:fltVal val="{v}"/>'
        except ValueError:
            return f'<p:strVal val="{v}"/>'

    return (
        f'<p:anim calcmode="lin" valueType="num">'
        f'{bhvr(ids, target, dur, [attr], ctn_extra=" fill=\"hold\"", additive=additive)}'
        f'<p:tavLst><p:tav tm="0"><p:val>{val(frm)}</p:val></p:tav>'
        f'<p:tav tm="100000"><p:val>{val(to)}</p:val></p:tav></p:tavLst></p:anim>'
    )


def anim_effect(ids: Ids, target: str, transition: str, filt: str, dur: int) -> str:
    return (
        f'<p:animEffect transition="{transition}" filter="{filt}">'
        f"{bhvr(ids, target, dur)}</p:animEffect>"
    )


def anim_clr(ids: Ids, target: str, attr: str, dur: int, rgb: str, child_style: bool = False) -> str:
    extra = ' override="childStyle"' if child_style else ""
    return (
        f'<p:animClr clrSpc="rgb" dir="cw">'
        f'{bhvr(ids, target, dur, [attr], extra=extra, ctn_extra=" fill=\"hold\"")}'
        f'<p:to><a:srgbClr val="{rgb}"/></p:to></p:animClr>'
    )


def set_str(ids: Ids, target: str, attr: str, dur: int, value: str) -> str:
    return (
        f'<p:set>{bhvr(ids, target, dur, [attr], ctn_extra=" fill=\"hold\"")}'
        f'<p:to><p:strVal val="{value}"/></p:to></p:set>'
    )


# Each effect: (preset class, preset id, subtype, builder(ids, target) -> behaviours XML,
# whether it takes a build group (grpId), extra cTn attributes).
def effect_def(kind: str, dur: int = 500, rgb: str = "DC2626", path: str = ""):
    D = dur
    defs = {
        # Entrances.
        "appear": ("entr", 1, 0, lambda i, t: set_vis(i, t, "visible"), True, ""),
        "fade": ("entr", 10, 0, lambda i, t: set_vis(i, t, "visible")
                 + anim_effect(i, t, "in", "fade", D), True, ""),
        "wipe": ("entr", 22, 8, lambda i, t: set_vis(i, t, "visible")
                 + anim_effect(i, t, "in", "wipe(left)", D), True, ""),
        "fly": ("entr", 2, 4, lambda i, t: set_vis(i, t, "visible")
                + anim(i, t, "ppt_x", D, "#ppt_x", "#ppt_x", "base")
                + anim(i, t, "ppt_y", D, "1+#ppt_h/2", "#ppt_y", "base"), True, ""),
        "zoom": ("entr", 53, 16, lambda i, t: set_vis(i, t, "visible")
                 + anim(i, t, "ppt_w", D, "0", "#ppt_w") + anim(i, t, "ppt_h", D, "0", "#ppt_h")
                 + anim_effect(i, t, "in", "fade", D), True, ""),
        # Exits.
        "appear-out": ("exit", 1, 0, lambda i, t: set_vis(i, t, "hidden"), True, ""),
        "fade-out": ("exit", 10, 0, lambda i, t: anim_effect(i, t, "out", "fade", D)
                     + set_vis(i, t, "hidden", D - 1), True, ""),
        "wipe-out": ("exit", 22, 2, lambda i, t: anim_effect(i, t, "out", "wipe(right)", D)
                     + set_vis(i, t, "hidden", D - 1), True, ""),
        "fly-out": ("exit", 2, 4, lambda i, t: anim(i, t, "ppt_x", D, "#ppt_x", "#ppt_x", "base")
                    + anim(i, t, "ppt_y", D, "#ppt_y", "1+#ppt_h/2", "base")
                    + set_vis(i, t, "hidden", D - 1), True, ""),
        "zoom-out": ("exit", 53, 16, lambda i, t: anim(i, t, "ppt_w", D, "#ppt_w", "0")
                     + anim(i, t, "ppt_h", D, "#ppt_h", "0")
                     + anim_effect(i, t, "out", "fade", D)
                     + set_vis(i, t, "hidden", D - 1), True, ""),
        # Emphasis.
        "fill-colour": ("emph", 1, 2, lambda i, t: anim_clr(i, t, "fillcolor", D, rgb)
                        + set_str(i, t, "fill.type", D, "solid")
                        + set_str(i, t, "fill.on", D, "true"), False, ""),
        "font-colour": ("emph", 3, 2, lambda i, t: anim_clr(i, t, "style.color", D, rgb, True),
                        True, ""),
        "line-colour": ("emph", 7, 2, lambda i, t: anim_clr(i, t, "stroke.color", D, rgb)
                        + set_str(i, t, "stroke.on", D, "true"), False, ""),
        "bold-flash": ("emph", 10, 0, lambda i, t: (
            '<p:anim calcmode="discrete" valueType="str">'
            + bhvr(i, t, D, ["style.fontWeight"], extra=' override="childStyle"',
                   ctn_extra=' fill="hold"')
            + '<p:tavLst><p:tav tm="0"><p:val><p:strVal val="normal"/></p:val></p:tav>'
            '<p:tav tm="50000"><p:val><p:strVal val="bold"/></p:val></p:tav>'
            '<p:tav tm="60000"><p:val><p:strVal val="normal"/></p:val></p:tav>'
            '<p:tav tm="100000"><p:val><p:strVal val="normal"/></p:val></p:tav>'
            "</p:tavLst></p:anim>"), True, ""),
        "bold-reveal": ("emph", 15, 0, lambda i, t: (
            "<p:set>" + bhvr(i, t, "indefinite", ["style.fontWeight"], extra=' override="childStyle"')
            + '<p:to><p:strVal val="bold"/></p:to></p:set>'), True, ""),
        "grow": ("emph", 6, 0, lambda i, t: (
            "<p:animScale>" + bhvr(i, t, D, ctn_extra=' fill="hold"')
            + '<p:by x="150000" y="150000"/></p:animScale>'), True, ""),
        "pulse": ("emph", 26, 0, lambda i, t: (
            '<p:animEffect transition="out" filter="fade">'
            + bhvr(i, t, D, ctn_extra=' tmFilter="0, 0; .2, .5; .8, .5; 1, 0"')
            + "</p:animEffect><p:animScale>"
            + bhvr(i, t, D // 2, ctn_extra=' autoRev="1" fill="hold"')
            + '<p:by x="105000" y="105000"/></p:animScale>'), True, ""),
        # Motion along a path, at uniform speed (no acceleration).
        "path": ("path", 0, 0, lambda i, t: (
            f'<p:animMotion origin="layout" path="{path}" pathEditMode="relative" ptsTypes="">'
            + bhvr(i, t, D, ["ppt_x", "ppt_y"], ctn_extra=' fill="hold"')
            + "</p:animMotion>"), True, ""),
    }
    return defs[kind]


class Effect:
    def __init__(self, kind: str, spid: int, *, para_index: int | None = None, **kw) -> None:
        self.kind, self.spid, self.para_index, self.kw = kind, spid, para_index, kw
        self.cls, self.preset, self.subtype, self.build, self.grouped, _ = effect_def(kind, **kw)

    def xml(self, ids: Ids, node_type: str, grp: dict[tuple[int, str], int]) -> str:
        ctn = ids()
        target = tgt(self.spid, self.para_index)
        grp_attr = ""
        if self.grouped:
            # PowerPoint keeps one effect per build group of a whole shape and
            # drops the others silently, so every effect on a shape gets its
            # own group; effects on paragraphs of one text build share one.
            if self.para_index is None:
                key = (self.spid, f"effect {ctn}")
            else:
                key = (self.spid, "exit" if self.cls == "exit" else "paragraphs")
            grp.setdefault(key, len([k for k in grp if k[0] == self.spid]))
            grp_attr = f' grpId="{grp[key]}"'
        return (
            f'<p:par><p:cTn id="{ctn}" presetID="{self.preset}" presetClass="{self.cls}" '
            f'presetSubtype="{self.subtype}" fill="hold"{grp_attr} nodeType="{node_type}">'
            '<p:stCondLst><p:cond delay="0"/></p:stCondLst>'
            f"<p:childTnLst>{self.build(ids, target)}</p:childTnLst></p:cTn></p:par>"
        )

    def duration(self) -> int:
        return self.kw.get("dur", 500)


def timing(clicks: list[list[list[Effect]]], *, auto_first: bool = False,
           text_shapes: set[int] = frozenset()) -> str:
    """clicks -> stages run one after another -> effects run together.
    With auto_first, the first click group starts with the slide."""
    ids = Ids()
    grp: dict[tuple[int, str], int] = {}
    groups = []
    for ci, stages in enumerate(clicks):
        outer = ids()
        start = '<p:cond delay="indefinite"/>'
        if ci == 0 and auto_first:
            start += '<p:cond evt="onBegin" delay="0"><p:tn val="2"/></p:cond>'
        inner = []
        offset = 0
        for si, effects in enumerate(stages):
            stage_id = ids()
            pars = []
            for ei, e in enumerate(effects):
                if ei > 0:
                    node = "withEffect"
                elif si == 0 and not (ci == 0 and auto_first):
                    node = "clickEffect"
                else:
                    node = "afterEffect"
                pars.append(e.xml(ids, node, grp))
            inner.append(
                f'<p:par><p:cTn id="{stage_id}" fill="hold"><p:stCondLst>'
                f'<p:cond delay="{offset}"/></p:stCondLst><p:childTnLst>{"".join(pars)}'
                "</p:childTnLst></p:cTn></p:par>"
            )
            offset += max(e.duration() for e in effects)
        groups.append(
            f'<p:par><p:cTn id="{outer}" fill="hold"><p:stCondLst>{start}</p:stCondLst>'
            f'<p:childTnLst>{"".join(inner)}</p:childTnLst></p:cTn></p:par>'
        )
    bld = []
    for (spid, kind), g in sorted(grp.items(), key=lambda kv: (kv[0][0], kv[1])):
        if spid in text_shapes and not kind.startswith("effect"):
            bld.append(f'<p:bldP spid="{spid}" grpId="{g}" build="p"/>')
        else:
            bld.append(f'<p:bldP spid="{spid}" grpId="{g}" animBg="1"/>')
    return (
        '<p:timing><p:tnLst><p:par><p:cTn id="1" dur="indefinite" restart="never" '
        'nodeType="tmRoot"><p:childTnLst><p:seq concurrent="1" nextAc="seek">'
        f'<p:cTn id="2" dur="indefinite" nodeType="mainSeq"><p:childTnLst>{"".join(groups)}'
        "</p:childTnLst></p:cTn>"
        '<p:prevCondLst><p:cond evt="onPrev" delay="0"><p:tgtEl><p:sldTgt/></p:tgtEl></p:cond>'
        '</p:prevCondLst><p:nextCondLst><p:cond evt="onNext" delay="0"><p:tgtEl><p:sldTgt/>'
        "</p:tgtEl></p:cond></p:nextCondLst></p:seq></p:childTnLst></p:cTn></p:par></p:tnLst>"
        f'<p:bldLst>{"".join(bld)}</p:bldLst></p:timing>'
    )


def caption(spid: int, text: str) -> str:
    return sp(spid, "caption", 30, 10, 900, 40, [text], fill=None, line=None, size=2000)


# --- §10.2 / §10.4: the effect vocabulary -----------------------------------


def effects() -> None:
    slides = []

    # Slide 1: entrances, then exits, one per click.
    names = ["appear", "fade", "wipe", "fly", "zoom"]
    shapes = caption(2, "Entrances, then exits (one per click)")
    clicks = []
    for i, n in enumerate(names):
        spid = 10 + i
        shapes += sp(spid, n, 40 + i * 180, 200, 150, 100, [n])
        clicks.append([[Effect(n, spid)]])
    for i, n in enumerate(names):
        clicks.append([[Effect(f"{n}-out", 10 + i)]])
    slides.append((slide_xml(shapes, timing(clicks)), []))

    # Slide 2: emphasis, one per click.
    names = ["fill-colour", "font-colour", "line-colour", "bold-flash", "bold-reveal",
             "grow", "pulse"]
    shapes = caption(2, "Emphasis (one per click)")
    clicks = []
    for i, n in enumerate(names):
        spid = 10 + i
        shapes += sp(spid, n, 30 + i * 128, 200, 115, 100, [n], line_w=3, size=1400)
        dur = 2000 if n != "pulse" else 500
        clicks.append([[Effect(n, spid, dur=dur)]])
    slides.append((slide_xml(shapes, timing(clicks)), []))

    # Slide 3: paragraph-level effects: build by paragraph, then a colour
    # change and an exit on single paragraphs.
    shapes = caption(2, "Paragraph-level effects")
    items = ["First point", "Second point (turns red)", "Third point (leaves)", "Fourth point"]
    paras = [text_p(t, 2400, "l") for t in items]
    shapes += sp(10, "bullets", 60, 100, 700, 300, paras, fill=None, line=None)
    clicks = [[[Effect("fade", 10, para_index=i)]] for i in range(4)]
    clicks.append([[Effect("font-colour", 10, para_index=1, dur=1000)]])
    clicks.append([[Effect("fade-out", 10, para_index=2)]])
    slides.append((slide_xml(shapes, timing(clicks, text_shapes={10})), []))

    # Slide 4: motion along a custom path.
    shapes = caption(2, "Motion path (uniform speed)")
    shapes += sp(10, "mover", 60, 300, 60, 60, geom="ellipse", fill="DC2626", line=None)
    path = "M 0 0 C 0.2 -0.4 0.4 -0.4 0.6 0 E"
    slides.append((slide_xml(shapes, timing([[[Effect("path", 10, dur=2000, path=path)]]])), []))

    # Slide 5: simultaneous shape swaps for paint changes with no native
    # effect (stroke width, dash): each old copy exits as its new copy enters,
    # three pairs on the same click.
    shapes = caption(2, "Simultaneous shape swaps (stroke width and dash)")
    clicks_stage = []
    for i in range(3):
        x = 80 + i * 280
        shapes += sp(20 + i, f"arrow{i} old", x, 200, 220, 80, geom="rightArrow",
                     fill="FFFFFF", line="1F2937", line_w=1)
        shapes += sp(30 + i, f"arrow{i} new", x, 200, 220, 80, geom="rightArrow",
                     fill="FFFFFF", line="DC2626", line_w=5, dash="dash")
        clicks_stage += [Effect("fade-out", 20 + i, dur=300), Effect("fade", 30 + i, dur=300)]
    # The new copies start hidden: an entrance effect hides its shape until it runs.
    slides.append((slide_xml(shapes, timing([[clicks_stage]])), []))

    # Slide 6: the same swap between two pictures, since graphics reach PPTX
    # as pictures (SPEC §9.4).
    shapes = caption(2, "Picture swap (a stroke change inside a drawing)")
    shapes += pic(20, "drawing old", 200, 200, 440, 160, "rId2")
    shapes += pic(30, "drawing new", 200, 200, 440, 160, "rId3")
    swap_dir = HERE / "pptx" / "swap"
    media = [("rId2", "state1.png", (swap_dir / "state1.png").read_bytes()),
             ("rId3", "state2.png", (swap_dir / "state2.png").read_bytes())]
    stage = [Effect("fade-out", 20, dur=300), Effect("fade", 30, dur=300)]
    slides.append((slide_xml(shapes, timing([[stage]])), media))

    pptx_package(HERE / "pptx" / "effects.pptx", slides)


# --- §10.3: chained motion paths for physical timing ------------------------

U, ANGLE, G = 12.0, math.radians(50), 9.81
T_FLIGHT = 2 * U * math.sin(ANGLE) / G
PLAY = 4.0  # seconds of playback for the whole flight
SCALE = 0.045  # slide widths per metre (and slide heights per metre * 16/9)
BALL = 30  # ball diameter in points
START_X, START_Y = 80.0, 480.0  # ball centre at launch, in points


def projectile(t: float) -> tuple[float, float]:
    """Position in metres at time t."""
    return U * math.cos(ANGLE) * t, U * math.sin(ANGLE) * t - G * t * t / 2


def to_slide(x: float, y: float) -> tuple[float, float]:
    """Metres to motion-path units (fractions of slide width and height, y down)."""
    return x * SCALE, -y * SCALE * (SLIDE_W / SLIDE_H)


def bezier_points(t0: float, t1: float) -> list[tuple[float, float]]:
    """The parabola between t0 and t1 as one exact cubic Bézier: start, two
    control points and end, in path units relative to the launch point."""
    p0 = to_slide(*projectile(t0))
    p3 = to_slide(*projectile(t1))
    # Quadratic control point from the tangents, raised to cubic.
    vx = U * math.cos(ANGLE)
    vy0 = U * math.sin(ANGLE) - G * t0
    dt = t1 - t0
    q = to_slide(projectile(t0)[0] + vx * dt / 2, projectile(t0)[1] + vy0 * dt / 2)
    c1 = (p0[0] + 2 / 3 * (q[0] - p0[0]), p0[1] + 2 / 3 * (q[1] - p0[1]))
    c2 = (p3[0] + 2 / 3 * (q[0] - p3[0]), p3[1] + 2 / 3 * (q[1] - p3[1]))
    return [p0, c1, c2, p3]


def bezier_segment(t0: float, t1: float, rel: tuple[float, float]) -> str:
    """bezier_points as a motion path relative to `rel`."""

    def f(p: tuple[float, float]) -> str:
        return f"{p[0] - rel[0]:.5f} {p[1] - rel[1]:.5f}"

    p0, c1, c2, p3 = bezier_points(t0, t1)
    return f"M {f(p0)} C {f(c1)} {f(c2)} {f(p3)} E"


def motion_chain() -> None:
    """A projectile (12 m/s at 50 degrees) as PowerPoint motion paths:
    slide 1, one path at uniform speed; slides 2 and 3, eight segments of
    equal time chained with 'after previous', written relative to the launch
    point (2) or to each segment's own start (3); slides 4 and 5, 4 and 16
    segments in the encoding of slide 2. Grey dots mark the true positions at
    every 1/16 of the flight. Each slide starts its animation on its own and
    advances after 6 seconds, so a video export shows all of them."""
    ox, oy = START_X - BALL / 2, START_Y - BALL / 2

    def ghosts(first_id: int) -> str:
        out = []
        for k in range(17):
            x, y = to_slide(*projectile(T_FLIGHT * k / 16))
            px = START_X + x * SLIDE_W / EMU_PER_PT
            py = START_Y + y * SLIDE_H / EMU_PER_PT
            out.append(sp(first_id + k, f"ghost{k}", px - 4, py - 4, 8, 8, geom="ellipse",
                          fill="9CA3AF", line=None))
        return "".join(out)

    def ball() -> str:
        return sp(10, "ball", ox, oy, BALL, BALL, geom="ellipse", fill="FF0000", line=None)

    def chained(n: int, relative_to_start: bool) -> list[list[Effect]]:
        stages = []
        seg = PLAY / n
        for k in range(n):
            t0, t1 = T_FLIGHT * k / n, T_FLIGHT * (k + 1) / n
            rel = to_slide(*projectile(t0)) if relative_to_start else (0.0, 0.0)
            stages.append([Effect("path", 10, dur=round(seg * 1000),
                                  path=bezier_segment(t0, t1, rel))])
        return stages

    slides = []
    single = [[Effect("path", 10, dur=round(PLAY * 1000),
                      path=bezier_segment(0, T_FLIGHT, (0.0, 0.0)))]]
    variants = [
        ("1 path, uniform speed", single),
        ("8 segments, paths relative to launch point", chained(8, False)),
        ("8 segments, paths relative to segment start", chained(8, True)),
        ("4 segments", chained(4, False)),
        ("16 segments", chained(16, False)),
    ]
    advance = (
        '<p:transition advClick="1" advTm="6000"/>'
    )
    for title, stages in variants:
        shapes = caption(2, title) + ghosts(100) + ball()
        xml = slide_xml(shapes, timing([stages], auto_first=True))
        xml = xml.replace("</p:clrMapOvr>", f"</p:clrMapOvr>{advance}")
        slides.append((xml, []))
    pptx_package(HERE / "pptx" / "motion-chain.pptx", slides)


# --- §8.3: OMML inside a14:m with an image fallback -------------------------

M_NS = "http://schemas.openxmlformats.org/officeDocument/2006/math"
A14_NS = "http://schemas.microsoft.com/office/drawing/2010/main"
MC_NS = "http://schemas.openxmlformats.org/markup-compatibility/2006"


def mr(text: str, italic: bool = True, sz: int = 2800) -> str:
    """A math run. PowerPoint does not size math runs from the paragraph, so
    each run carries its size."""
    sty = "" if italic else '<m:rPr><m:sty m:val="p"/></m:rPr>'
    return (
        f'<m:r>{sty}<a:rPr lang="en-GB" sz="{sz}" i="{1 if italic else 0}">'
        '<a:latin typeface="Cambria Math" panose="02040503050406030204" pitchFamily="18" '
        'charset="0"/></a:rPr>'
        f"<m:t>{escape(text)}</m:t></m:r>"
    )


def frac(num: str, den: str) -> str:
    return f"<m:f><m:fPr><m:ctrlPr/></m:fPr><m:num>{num}</m:num><m:den>{den}</m:den></m:f>"


def ssup(base: str, sup: str) -> str:
    return f"<m:sSup><m:e>{base}</m:e><m:sup>{sup}</m:sup></m:sSup>"


def ssub(base: str, sub: str) -> str:
    return f"<m:sSub><m:e>{base}</m:e><m:sub>{sub}</m:sub></m:sSub>"


def ssubsup(base: str, sub: str, sup: str) -> str:
    return f"<m:sSubSup><m:e>{base}</m:e><m:sub>{sub}</m:sub><m:sup>{sup}</m:sup></m:sSubSup>"


def nary(op: str, sub: str, sup: str, body: str) -> str:
    return (
        f'<m:nary><m:naryPr><m:chr m:val="{op}"/><m:ctrlPr/></m:naryPr>'
        f"<m:sub>{sub}</m:sub><m:sup>{sup}</m:sup><m:e>{body}</m:e></m:nary>"
    )


def delim(inner: str, beg: str = "(", end: str = ")") -> str:
    return (
        f'<m:d><m:dPr><m:begChr m:val="{beg}"/><m:endChr m:val="{end}"/><m:ctrlPr/></m:dPr>'
        f"<m:e>{inner}</m:e></m:d>"
    )


def matrix(rows: list[list[str]]) -> str:
    mrs = "".join("<m:mr>" + "".join(f"<m:e>{c}</m:e>" for c in row) + "</m:mr>" for row in rows)
    return f"<m:m>{mrs}</m:m>"


EQUATIONS = [
    # (label, OMML content); pptx/math-fallback/fallback.typ draws the fallbacks.
    (
        "fraction, sub- and superscripts",
        mr("v") + mr("=", False) + frac(mr("Δ", False) + mr("s"), mr("Δ", False) + mr("t"))
        + mr(",", False) + mr("  ", False) + ssubsup(mr("x"), mr("1", False), mr("2", False))
        + mr("+", False) + ssub(mr("y"), mr("0", False))
        + mr("+", False) + ssup(mr("z"), mr("n")),
    ),
    (
        "n-ary sum",
        nary("∑", mr("i") + mr("=", False) + mr("1", False), mr("n"), mr("i"))
        + mr("=", False) + frac(mr("n") + delim(mr("n") + mr("+", False) + mr("1", False)),
                                mr("2", False)),
    ),
    (
        "matrix in delimiters",
        mr("A") + mr("=", False) + delim(matrix([[mr("a"), mr("b")], [mr("c"), mr("d")]])),
    ),
]


def fallback_png(index: int) -> bytes:
    path = HERE / "pptx" / "math-fallback" / f"eq{index}.png"
    return path.read_bytes()


FOLIO_TEST_NS = "urn:folio:test:unsupported"


def alternate(choice: str, fallback: str, requires: str) -> str:
    """An mc:AlternateContent as PowerPoint writes it for math. `requires` is
    "a14", or "ftest", a namespace no application knows, which forces the
    fallback."""
    ns = A14_NS if requires == "a14" else FOLIO_TEST_NS
    return (
        f'<mc:AlternateContent xmlns:mc="{MC_NS}">'
        f'<mc:Choice xmlns:{requires}="{ns}" Requires="{requires}">{choice}</mc:Choice>'
        f"<mc:Fallback>{fallback}</mc:Fallback></mc:AlternateContent>"
    )


def math_slide(requires: str, title: str) -> tuple[str, list[tuple[str, str, bytes]]]:
    shapes = caption(2, title)
    media = []
    for i, (label, omml) in enumerate(EQUATIONS, start=1):
        spid = 10 + i
        y = 70 + (i - 1) * 120
        rid = f"rId{i + 1}"
        media.append((rid, f"eq{i}.png", fallback_png(i)))
        choice = sp(
            spid, f"Equation {i}", 60, y, 640, 100,
            [
                '<a:p><a:pPr algn="ctr"/>'
                f'<a14:m xmlns:a14="{A14_NS}"><m:oMathPara xmlns:m="{M_NS}"><m:oMathParaPr>'
                f'<m:jc m:val="centerGroup"/></m:oMathParaPr><m:oMath>{omml}</m:oMath>'
                '</m:oMathPara></a14:m><a:endParaRPr lang="en-GB" sz="2800"/></a:p>'
            ],
            fill=None, line=None,
        )
        fallback = sp(spid, f"Equation {i}", 60, y, 640, 100, [""], line=None, blip=rid)
        shapes += alternate(choice, fallback, requires)
        shapes += sp(40 + i, f"label {i}", 720, y + 30, 220, 40, [label], fill=None, line=None,
                     size=1200)
    # Inline math between ordinary runs in one paragraph.
    inline = (
        '<a:p><a:pPr algn="l"/><a:r><a:rPr lang="en-GB" sz="2400"/>'
        "<a:t>Kinetic energy is </a:t></a:r>"
        f'<a14:m xmlns:a14="{A14_NS}"><m:oMath xmlns:m="{M_NS}">'
        + ssub(mr("E", sz=2400), mr("k", False, 2400)) + mr("=", False, 2400)
        + frac(mr("1", False, 2400), mr("2", False, 2400)) + mr("m", sz=2400)
        + ssup(mr("v", sz=2400), mr("2", False, 2400))
        + '</m:oMath></a14:m><a:r><a:rPr lang="en-GB" sz="2400"/>'
        "<a:t> in joules.</a:t></a:r></a:p>"
    )
    rid = f"rId{len(EQUATIONS) + 2}"
    media.append((rid, "eq4.png", fallback_png(4)))
    choice = sp(20, "Inline equation", 60, 440, 640, 80, [inline], fill=None, line=None)
    fallback = sp(20, "Inline equation", 60, 440, 640, 80, [""], line=None, blip=rid)
    shapes += alternate(choice, fallback, requires)
    shapes += sp(44, "label 4", 720, 460, 220, 40, ["inline, between runs"], fill=None,
                 line=None, size=1200)
    return slide_xml(shapes), media


def math_a14() -> None:
    """Three display equations and one inline equation, each written as
    PowerPoint 2010 writes them: an mc:AlternateContent whose Choice
    (Requires="a14") holds the OMML in a14:m and whose Fallback is the same
    shape filled with a picture of the equation. The fallback pictures are
    red and labelled, so a screenshot shows which branch a viewer took.
    Slide 2 repeats slide 1 with a Choice that requires an unknown
    namespace, so every viewer, PowerPoint included, shows the fallback."""
    pptx_package(
        HERE / "pptx" / "math-a14.pptx",
        [
            math_slide("a14", "OMML in a14:m with image fallback (SPEC §8.3)"),
            math_slide("ftest", "The same, with the fallback forced"),
        ],
    )


def main() -> None:
    columns_balance()
    baseline_grid()
    numbering_gaps()
    effects()
    motion_chain()
    math_a14()


if __name__ == "__main__":
    main()
