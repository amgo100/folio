# Office assumption fixtures

Hand-written DOCX and PPTX files that test the Office assumptions of SPEC §16.2 (issue #37), the scripts that measure what Word and PowerPoint make of them, and the results. The results drove the revisions of §7.3, §10.3, §10.4, §11.2 and §13.2.

## Files

| File | Tests | Section |
| --- | --- | --- |
| `docx/columns-balance.docx` | Column sets closed by continuous section breaks: mid-page (set A, and again with widow control off as set AW), across a page break (B), three columns with a rule (C), and at the end of the document followed by an empty continuous section (D) | §13.2 |
| `docx/columns-end.docx` | Control: a column set at the end of the document with no section after it (E) | §13.2 |
| `docx/baseline-grid.docx` | Body text with exact 13pt leading on a 13pt grid. Blocks that are whole grid lines tall, padded to whole lines, or not; 8pt and 12pt text in 13pt lines; a two-column section; a page that starts with a heading (page break before); then auto spacing with Word's document grid, and exact spacing with no grid; last, a heading pushed to the top of a page by a natural page break | §7.3 |
| `docx/numbering-gaps.docx` | An exam with hidden questions and parts, exported three ways (lists X, Y and Z); chapter-numbered figure captions with `STYLEREF` and `SEQ \s 1`; `SEQ` resets with `\r n`; a gapped chapter number | §11.2 |
| `pptx/effects.pptx` | Entrances and exits (slide 1), emphasis effects (2), paragraph-level effects (3), a custom motion path (4), simultaneous swaps of three shape pairs (5), and a swap of two pictures (6) | §10.2, §10.4 |
| `pptx/motion-chain.pptx` | A projectile (12 m/s at 50°, played over 4 s) as one motion path (slide 1), as 8 chained segments written relative to the launch point (2) or to each segment's start (3), and as 4 (4) and 16 (5) segments | §10.3 |
| `pptx/math-a14.pptx` | Three display equations (fraction, scripts, n-ary sum, matrix in delimiters) and one inline equation as OMML in `a14:m` with a picture fallback (slide 1); the same with a `Choice` that no application supports, which forces the fallback (slide 2). The fallback pictures are red and labelled "image fallback" | §8.3 |

`pptx/math-fallback/fallback.typ` and `pptx/swap/swap.typ` draw the pictures (`typst compile --ppi 150 fallback.typ eq{p}.png`, `typst compile --ppi 150 swap.typ state{p}.png`). Every numbered paragraph in `numbering-gaps.docx` says which label it expects.

## Running

Regenerate the fixtures (standard library Python; the output is byte-identical on every run):

```
python tests/office/generate.py
```

Measure them in Word and PowerPoint on Windows. The probes drive Office through COM; the PowerPoint probe also renders `motion-chain.pptx` to video and extracts a frame every 100 ms through the Windows media composition API. Then summarise:

```
powershell -File tests/office/probe-word.ps1 -Out <dir>
powershell -File tests/office/probe-powerpoint.ps1 -Out <dir>
python tests/office/analyse.py <dir>
```

The Word probe exports each document to XPS, whose glyph origins give exact baselines, and to PDF for viewing. It reads list labels and field results before and after a field update, and inserts a question in two places. The PowerPoint probe reads every shape and effect back through the object model, exports the slides as PNG, saves a copy and reads that again.

## Results

Word and PowerPoint 16.0 (Microsoft 365), build 20527, on Windows 11, November 2026. Every file opened through COM without an error or a repair.

### Column balancing (§13.2): confirmed

| Set | Lines per column on the set's last page |
| --- | --- |
| A, mid-page | 16, 12 |
| AW, set A with widow control off | 15, 15 |
| B, across a page break | 50 and 51 on full pages; 7, 5 on the last page |
| C, three columns | 17, 15, 15 |
| D, end of document, empty continuous section after | 13, 10 |
| E, end of document, no section after | 22, 0 |

Word balances before a continuous section break, including the last page of a set that runs across pages. It balances by its own line breaks and keeps widow and orphan control (on by default), so columns of whole paragraphs can differ by a few lines. With widow control off they come out equal. Without a section after it, a set at the end of the document is not balanced, so the extra continuous section of §13.2 is needed.

### Baseline grid (§7.3): confirmed, with padding

Baselines measured from the XPS export, against the 13pt grid through the first body line (XPS rounds origins to 0.1pt):

- Body lines with exact 13pt spacing stay on the grid, across pages and in both columns of a two-column section. Lines of 8pt and 12pt text in 13pt lines keep the same baseline.
- A block that is a whole number of grid lines keeps the following text on the grid: a heading with 13pt before and one 13pt line, and a heading of 6pt + 17pt + 3pt after.
- A block that is not (6pt + 17pt) moves every following line 3pt off the grid until the end of the page. Word does not snap back.
- The document grid makes no difference under exact spacing: the same page with no `w:docGrid` gives the same baselines. With auto spacing it rounds line heights up to the grid, but space before still moves text off the grid (5.5pt after a heading with 6pt before).
- Word drops space before at the top of a page, both for a heading with `pageBreakBefore` and for one pushed there by a natural page break. Padding that `grid-snap-align` puts above a block that starts a page is therefore lost.

The approximation therefore holds when the writer makes every block a whole number of grid lines, which §7.3 now specifies. It needs Folio's line count for each block. Where Word breaks lines differently, the block's height changes and the following lines are off the grid until the next page.

### Gapped numbering (§11.2): confirmed, with instance boundaries

All 17 `STYLEREF` and `SEQ` results are right as opened and after `Fields.Update`: Figure 1.1, 1.2, 1.4 (`\r 4`), 2.1, 2.2, 4.1, 4.3 (`\r 3`); Table 1, 3, 4. The heading after a hidden chapter, in a list instance with a start override, is 4, and `STYLEREF 1 \s` returns 4.

List labels, against the expected 1. (a) (b) 2. (a) 4. (a) (c) 5. (a) 7. (a):

- **List X** (override only the gapped level) and **list Y** (override the gapped level and the levels above): Question 5's part (a) shows "(c)". A start override sets the level's start for its whole instance, so after the part 4(b) gap level 1 restarts at 3 for every later question in that instance.
- **List Z** (as Y, and Question 5 starts another instance): every label as expected. This is the encoding §11.2 now specifies.
- Word continues numbering across instances of one abstract list: in list X, Question 5 is 5 without an override on level 0.
- A question inserted after 2(a) (Enter at its end, then Shift+Tab) is 3, and Question 4 stays 4. One inserted at the start of Question 4 (Enter before its text) takes that instance's start, 4, and Question 4 becomes 5.

### OMML in `a14:m` (§8.3): confirmed in PowerPoint; LibreOffice and Keynote not tested

PowerPoint takes the `a14:m` branch for all four equations, inline math between runs included. It renders them as native equations (`math-a14/slide1.png` in the probe's output directory) and keeps both the OMML and the fallback when it saves. Slide 2 shows the fallback pictures, so the fallback markup is valid. Math runs need their own size (`a:rPr sz`); they do not take it from the paragraph.

Not tested: LibreOffice and Keynote (neither available on the test machine), and PowerPoint versions before Microsoft 365. §16.2 keeps the LibreOffice and Keynote row open.

### Effect vocabulary (§10.2, §10.4): confirmed

Every effect reads back as its native preset, with its trigger, duration and paragraph, and is unchanged after PowerPoint saves the file:

| §10.2 | PowerPoint effect (preset class, id) |
| --- | --- |
| appear, fade, wipe, fly-in, zoom (entrance and exit) | Appear (1), Fade (10), Wipe (22), Fly (2), Zoom (53) |
| colour change | Change Fill Colour (emph 1), Change Font Colour (emph 3), Change Line Colour (emph 7) |
| bolding | Bold Flash (emph 10), Bold Reveal (emph 15) |
| growing | Grow/Shrink (emph 6) |
| pulsing | Pulse (emph 26) |
| motion along a path | custom path (path 0) |
| paragraph-level steps | effects on `p:txEl/p:pRg`, with `bldP build="p"` |
| shape swap | Fade exit and Fade entrance "with previous", for three shape pairs and for two pictures on one click |

### Chained motion paths (§10.3): confirmed

Ball centre in the video against the physical position, in points on a 960 × 540pt slide; the flight spans 624 × 186pt:

| Slide | Max | Mean |
| --- | --- | --- |
| 1 path | 41.8 | 27.9 |
| 4 segments | 12.7 | 7.6 |
| 8 segments, relative to the launch point | 5.2 | 1.8 |
| 8 segments, relative to each segment's start | 531 | 290 |
| 16 segments | 2.8 | 1.4 |

- PowerPoint moves along a path at uniform speed in path units, which are fractions of the slide's width and height. On a 16:9 slide vertical travel therefore counts for more than horizontal travel. This model matches every slide to within the measurement noise of about 1.5pt; uniform speed in points does not (12.9pt mean on slide 1).
- Paths are relative to the shape's original position, not to where the previous effect left it. Slide 3, written relative to each segment's start, ends 570pt from the landing point.
- The writer can predict the deviation. For this flight it is 43.9, 12.5, 3.2, 0.8 and 0.2pt for 1, 4, 8, 16 and 32 segments, falling with the square of the number of segments.

## Notes for the writers

What a writer must do that the specification does not spell out:

- **PPTX build groups.** Every effect on a whole shape needs its own build group: a `grpId` on the effect and a `bldP` for it. When two effects on one shape share a group, PowerPoint keeps the first and drops the rest without a repair prompt. Effects on paragraphs of one text build share a group with `build="p"`.
- **Effect XML.** The behaviours of each preset follow what PowerPoint writes for it (`generate.py`, `effect_def`). PowerPoint maps them back to its presets by `presetClass` and `presetID`.
- **First automatic effect.** An effect that starts with the slide has `nodeType="afterEffect"` in a click group whose start conditions are `delay="indefinite"` and `evt="onBegin"` on the main sequence.
- **DOCX compatibility mode.** `w:compatSetting compatibilityMode=15` keeps Word out of compatibility mode, whose layout differs.
- **Office COM from PowerShell.** A Word `Paragraph` object follows its paragraph mark, so after a split it refers to the second half. PowerShell variable names are case-insensitive, so `$s` and `$S` are the same variable.
