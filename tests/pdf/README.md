# PDF assumption fixtures

Test files for the PDF assumptions of SPEC §16.2 (issue #38), the krilla prototype that writes them, and the results. The results drove the revisions of §8.3, §10.4, §11.1, §15 and §16.2.

## Files

| File | Tests | Section |
| --- | --- | --- |
| `figure/figure.typ` | A vector figure with text (axes, a curve, three labels), compiled by Typst 0.15 to `figure-1.7.pdf` and `figure-2.0.pdf` | §11.1 |
| `out/figure-embed.pdf` | The figure placed with krilla's `Surface::draw_pdf_page`, PDF 1.7, no validator, tagged as a `Figure` with alt text | §11.1 |
| `out/figure-embed-2.0-in-2.0.pdf` | The PDF 2.0 figure in PDF 2.0 output | §11.1 |
| `out/figure-svg-ua1-a2u.pdf` | The figure converted to SVG by `hayro-svg` and drawn with `krilla-svg`, under krilla's PDF/UA-1 and PDF/A-2u validators | §11.1 |
| `out/figure-svg-a4.pdf` | The same under PDF/A-4 (PDF 2.0) | §11.1 |
| `out/math-alt-a4.pdf` | "The value is x² + 1." with the formula tagged `Formula` with alt text only; PDF 2.0, PDF/A-4, declared PDF/UA-2 and WTPDF 1.0 | §8.3 |
| `out/math-se-a4.pdf` | The same with MathML structure elements under `Formula` (`math` › `mrow` › `msup`(`mi`, `mn`), `mo`, `mn`), in the MathML namespace | §8.3 |
| `out/math-af-a4.pdf` | The same with a MathML associated file on `Formula` (`AFRelationship /Supplement`, `application/mathml+xml`) under base PDF/A-4 | §8.3, §11.1 |
| `out/math-af-a4f.pdf` | The associated file under PDF/A-4f | §8.3, §11.1 |
| `out/math-both-a4f.pdf` | Structure elements and the associated file under PDF/A-4f | §8.3 |
| `out/transitions.pdf` | Eleven 4:3 slides of two step pages each, one per transition style (`/Trans /S` Split, Blinds, Box, Wipe, Dissolve, Glitter, Fly, Push, Cover, Uncover, Fade; `/D 3`), on the first step page only; light and dark slides alternate; page labels 1a, 1b, 2a… as §11.1 | §10.4 |

The prototype also tries four combinations that krilla refuses, so they have no file: the figure embedded under PDF/UA-1, PDF/A-2b and PDF/A-4f, and the PDF 2.0 figure in PDF 1.7 output.

## Running

The prototype is a crate of its own (`prototype/`, with its own `[workspace]` and lockfile), so that krilla does not enter the main workspace before `folio-pdf` chooses its version. It writes the fixtures, and the output is byte-identical on every run:

```
cd tests/pdf/prototype
cargo run --release -- ../out
```

Then validate them with veraPDF and compare against the results below:

```
python tests/pdf/validate.py [--verapdf <command>]
```

krilla cannot write MathML structure elements, associated files on structure elements, the PDF/UA-2 identification or page transitions. The prototype writes the rest with krilla and adds these afterwards with `lopdf`, as a krilla extension would. For the math files it also corrects two krilla 0.8.2 defects that its unreleased `main` fixes (see below), so that veraPDF's results show the math rules and nothing else.

Versions: krilla 0.8.2 (the version Typst 0.15 uses) with `hayro-write` 0.7.0, `krilla-svg` 0.8.1, `hayro-svg` 0.9.0, `lopdf` 0.47.0; veraPDF 1.30.4; Typst 0.15.0. Windows 11, November 2026.

## Results

### krilla embeds PDF figures as form XObjects (§11.1): confirmed, but not under PDF/A or PDF/UA

- **Form XObject.** `draw_pdf_page` writes the page as one form XObject that keeps the page's content stream, its fonts and its text operators (`out/figure-embed.pdf`: one form XObject, with a Font resource and `BT`/`TJ`; no image XObject). Text in the figure stays text: "t / s", "v" and "projectile" extract from the PDF. `hayro-write` copies the page's content and resources; it drops annotations, the structure tree and `/StructParents`.
- **Refused under every validator.** krilla registers `ValidationError::EmbeddedPDF` for every embedded page and every PDF/A level and PDF/UA-1 prohibit it, "because we cannot manually verify whether the file actually fulfills all the criteria". The prototype gets `EmbeddedPDF` under PDF/UA-1, PDF/A-2b and PDF/A-4f; Typst 0.15 reports the same as "embedding PDFs is currently not supported in this export mode".
- **The refusal is justified for PDF/UA.** Validated anyway, `figure-embed.pdf` fails PDF/UA-1 rules 7.1-1 and 7.1-2: the figure's own marked content (its artifacts and its tagged text, now without a structure tree) nests inside Folio's tagged figure. The PDF/A-2b failures (6.2.4.3-2 DeviceRGB, 6.2.10-2 transparency) prove nothing about the embedding: the file was written without a validator, so it has no output intent at all. Neither do the identification and `DisplayDocTitle` failures (5-1, 6.6.4-1, 7.1-10). For PDF/A the reason is krilla's: it cannot check an embedded page's fonts and colours.
- **Conversion passes.** Converted to SVG with `hayro-svg` and drawn with `krilla-svg`, the same figure passes krilla's UA-1 and A-2u validators and veraPDF's PDF/UA-1, PDF/A-2u and PDF/A-4 profiles, and renders identically. Its text becomes outlines, so the figure's content reaches assistive technology only through its alt text, which a PDF/UA figure needs anyway.
- **Version.** A PDF 2.0 figure in PDF 1.7 output is an error (`PdfError::VersionMismatch`); in PDF 2.0 output it embeds.

### MathML structure elements and associated files (§8.3, §11.1): confirmed

| File | PDF/A-4 or 4f | PDF/UA-2 | WTPDF 1.0 accessibility |
| --- | --- | --- | --- |
| `math-alt-a4.pdf` (alt text only) | pass (4) | pass | pass |
| `math-se-a4.pdf` (structure elements) | pass (4) | pass | pass |
| `math-af-a4.pdf` (associated file) | **fail (4), rule 6.9-3** | pass | pass |
| `math-af-a4f.pdf` (associated file) | pass (4f) | pass | pass |
| `math-both-a4f.pdf` (both) | pass (4f) | pass | pass |

- **Structure elements are valid everywhere PDF 2.0 is.** MathML is the one domain-specific namespace of ISO 32000-2 (§14.8.6.3); its elements need no role map, and the PDF/UA-2 and WTPDF namespace rules (8.2.4) accept them. veraPDF's only math rule, 8.2.5.29-1, requires `math` to be a child of `Formula`.
- **Associated files need PDF/A-4f.** Base PDF/A-4 (ISO 19005-4:2020) allows only PDF/A files as embedded files; veraPDF's rule 6.9-3 parses every embedded file as a PDF, so a MathML file fails. PDF/A-4f and 4e omit the rule; 4f also needs an `EmbeddedFiles` entry in the catalog's name dictionary (6.9-5), which krilla's A-4f writes. PDF/A-2 forbids non-PDF/A embedded files the same way (rule 6.8-5); PDF/A-3 allows any. A dated revision of ISO 19005-4 (DIS 19005-4.2, at the close of voting in late 2026) is to let MathML files added for accessibility into base PDF/A-4.
- **Alt text alone is not PDF/UA-2.** PDF/UA-2 8.2.5.29 requires mathematics to carry MathML, as structure elements or as an associated file on `Formula`; alt text or `ActualText` suffices only for formulae that are not mathematics. veraPDF does not check this, so `math-alt-a4.pdf` passes while not conforming.
- **Readers.** NVDA (2026.1 and later, with MathCAT built in) reads MathML structure elements in Acrobat Reader, and both structure elements and associated files in Foxit and Firefox; Acrobat does not read associated files (NVDA user guide; PDF Association, "Accessible math in PDF, finally", March 2026). JAWS does not read MathML in PDF yet. The PDF Association's best-practice guide recommends writing both where the conformance level allows. Not tested here: no screen reader was installed.
- **krilla 0.8.2 cannot write either.** `TagKind` is closed and has no MathML namespace; associated files are document-level only (`Document::embed_file`); there is no PDF/UA-2 validator (krilla issue #449, opened October 2026; a Typst developer has started UA-2). `pdf-writer` 0.15, which krilla writes with, has the primitives: `Namespace::mathml_3_0_ns`, `StructElement::namespace` and `StructElement::associated_files`.
- **krilla 0.8.2's PDF 2.0 tags fail PDF/UA-2.** `pdf-writer` 0.15.0 writes the PDF 2.0 namespace as `https://www.iso.org/pdf2/ssn` instead of `http://iso.org/pdf2/ssn`, so veraPDF treats every element as non-standard (8.2.4-1), and krilla keeps the root `Document` in the PDF 1.7 namespace (8.2.5.2-2). Both are fixed on the unreleased `main` branches (pdf-writer's `structure.rs`; krilla PR #450). Outline destinations are not structure destinations (8.8-1), so the math files have no outline.

### Page transitions in classroom viewers (§10.4): does not hold

Acrobat on Windows was driven on screen with `out/transitions.pdf`; the other rows come from the viewers' source code and documentation.

| Viewer | `/Trans` | Evidence |
| --- | --- | --- |
| Chrome built-in viewer (PDFium), Chromebooks | No | PDFium's public API has no transition accessor |
| Microsoft Edge | No | No presentation mode; moved to an Adobe engine in 2025 |
| Firefox (pdf.js), including Presentation Mode | No | `web/pdf_presentation_mode.js` has no transition code |
| macOS Preview, iPadOS and iOS (Files, Books, Quick Look) | No | PDFKit exposes no transition |
| Acrobat and Reader on macOS | In practice no | Users report transitions ignored since Big Sur, still in September 2026 |
| Acrobat and Reader on Windows | Yes, full screen only | Observed in Acrobat 26.003 on Windows 11 (below). Can be turned off ("Ignore all transitions"); a user default applies to pages without `/Trans` |
| Foxit, PDF-XChange (Windows) | Yes, full screen only | Same preferences as Acrobat |
| SumatraPDF | No | Presentation mode cuts; the request is open |
| Okular, GNOME Evince and Papers | Yes, presentation mode only | Okular lacks Fly, Push, Cover and Uncover |
| pdfpc, BeamerPresenter | Yes | pdfpc lacks Fly |
| Impressive, Présentation.app | No | Their own transitions, or none |
| SMART Notebook, ActivInspire | No | PDF pages are imported as images or objects |
| Google Drive, Teams and OneDrive previews, PDF Expert, Xodo | No (no evidence of support) | No presentation mode |

**Acrobat on Windows, observed** (Acrobat 26.003, Windows 11, default preferences, 25 November 2026):

- In full screen (Ctrl+L), all eleven styles played, with the declared dimension and direction (horizontal Split and Blinds, Box outward, Wipe, Glitter, Fly, Push, Cover and Uncover from the left) and close to the declared 3 seconds. Fly started after a short pause.
- Step pages without `/Trans` cut.
- A page reached through Go to Page plays its transition too, not only one reached by Page Down.
- In normal view every page change cuts.
- Page labels 1a, 1b… show in the page box and are accepted by Go to Page.

The viewers schools use most (browser viewers, Chromebooks, Apple devices, interactive whiteboard software) never show a transition; desktop full-screen modes on Windows and Linux do. §10.4 now treats the transition as an enhancement that slides must not depend on. Two details for the writer:

- **Default transitions.** Acrobat, Foxit, PDF-XChange, Okular and pdfpc can apply a user's default transition to pages without `/Trans`, so step pages may animate whatever Folio writes. Writing `/Trans /S /R` (replace) on step pages probably stops Acrobat's default (its documentation says the default applies to pages without a transition; not tested), but not Okular's or pdfpc's.
- **Page labels.** pdfpc and BeamerPresenter treat consecutive pages with the same page label as one slide, for overview and navigation. Folio's step labels (3a, 3b, §11.1) are all different, so these tools show each step as a slide.

krilla 0.8.2 writes no `/Trans` (its page settings have none); PDF/A and PDF/UA allow it (PDF/A-2 forbids only `PresSteps`). The prototype adds it afterwards.

## Test corpus (§11.1)

`../corpus/` (`tests/corpus`, shared with the other corpus tests; see its README) holds the documents that Folio's PDF output is checked with (issue #32): `notes` (three A5 pages of physics notes: headings at three levels, all three kinds of list, a sidenote, a figure with a caption, a code block, hyphenation across a page break), `exam` (an exam paper in two variants: the paper, two A4 pages of instructions and numbered questions, also built for DOCX, which has no writer yet; and its mark scheme, which adds answers computed with `calc` and sets the text smaller), `polish` (Polish text with its quotation marks, hyphenation and a line that repeats an explicit hyphen), `figures` (three A4 pages with an image of each format Folio reads, issue #47: a PNG, a JPEG passed through, an SVG drawn as vector paths, and a PDF figure converted to vector paths as PDF/UA-1 requires, in numbered figures with captions, and a WebP and a GIF in a line of text; `tests/images/generate.py` writes them; and, for issues #300 and #304, an SVG with a filter, rotations and oriented arrowheads, and a PDF figure with calibrated colour, a spot colour whose calculator function amplifies the last bits of its maths, and shadings, written by `assets/spectrum.py`), and `acceptance` (the acceptance exam of SPEC §15, four A4 pages of an A-level physics paper with maths, quantities, numbered questions, figures as SVG and PNG, references, page references and footnotes; `tests/README.md` describes it).

`cargo test -p folio --test pdf` compiles each, in every variant it builds as PDF (written as `<project>-<variant>.pdf`), checks that krilla's PDF/UA-1 validator accepted it, that it has the structure elements, metadata and bookmarks it should, and that it compiles to the same bytes in a new world. That test is part of `scripts/check.sh`, as is `cargo test -p folio --test determinism`, which builds the corpus in two differently set up processes, compares the bytes, and compares their digests with the recorded ones (§2.4). With `FOLIO_PDF_OUT=<dir>` it also writes each PDF and the text a reader should extract from it (the document's paragraphs, one per line).

```
python tests/pdf/corpus.py [--verapdf <command>] [--pdftotext <command>] [--out <dir>]
```

runs that test and checks every PDF with veraPDF's PDF/UA-1 profile and with Poppler's `pdftotext -raw`, whose output must be the expected text: a soft hyphen at a line break joins the lines around it, a line that ends in a hyphen after a letter joins the next without a space, and other white space is one space. It needs Java and Poppler; it is not part of `scripts/check.sh`.

Results (veraPDF 1.28.2, Poppler 24, Linux, November 2026): all three documents pass PDF/UA-1 (106 rules) and their text round-trips. These predate the exam's mark scheme variant, whose PDF has so far passed krilla's validator and the `pdftotext` round-trip only.

Two behaviours of readers shaped the writer:

- A hyphen that a line break inserts and a hyphen typed in the text are the same glyph, and a ToUnicode map gives a glyph one text. So every hyphen maps to `-`, and the span of an inserted hyphen has the actual text U+00AD (a repeated Polish hyphen has an empty one). Mapping the inserted hyphen's glyph to U+00AD instead made krilla give the other uses of the glyph actual text of their own, which overrode the empty actual text of a repeated hyphen.
- `pdftotext` ignores the structure tree, so artifacts are extracted too, but honours actual text, which it puts on a line of its own.

## Dependency maintenance (§2.2), November 2026

| Crate | Latest release | Activity |
| --- | --- | --- |
| `harfrust` | 0.16.1, 2026-11-19 | HarfBuzz team; weekly commits |
| `icu_segmenter` | 2.4.0, 2026-10-22 | Unicode ICU4X team; daily commits |
| `salsa` | 0.28.6, 2026-11-22 | salsa-rs, used by rust-analyzer and ty; still 0.x |
| `lsp-server` | 0.10.0, 2026-07-16 | rust-lang (rust-analyzer); moved to `gen-lsp-types` in 2026 |
| `krilla` | 0.8.2, 2026-06-04 | Six months without a release, but `main` is active (48 commits since September); the maintainer reports little review time |

All five are maintained. §15 now makes this review part of every milestone's exit.
