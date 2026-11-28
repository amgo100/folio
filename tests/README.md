# Tests

How Folio is tested (SPEC §14.1, §14.2): what lives where, what runs when,
and how to update expected output. There is no hosted CI; every check runs
from the scripts in `scripts/`.

## Layout

| Path | What it holds |
| --- | --- |
| `tests/corpus/<project>/` | The test corpus: whole projects (`folio.toml`, content, stylesheets), each covering features a real document uses. Every build of every project goes through the golden, PDF, determinism and command-line tests. |
| `tests/golden/layout/` | Layout golden files: `<project>-<variant>.<target>.txt` for each corpus build that goes through layout, a text dump of its pages and fragments, and `<project>-<variant>.<target>-<page>.png` for each of its pages, drawn by `folio-render`; `boxes.pdf.*` for a document with fixed-size object boxes; `footnotes.pdf.*` for a document with footnotes numbered per page, one of them continued. |
| `crates/folio-testkit/` | The test infrastructure as a crate: corpus loading, golden-file checks and dumps, the fuzz harnesses, the reference document generator, the benchmarks. |
| `crates/folio-testkit/benches/compile.rs` | The instruction-count benchmarks of the regression gate. |
| `crates/folio/tests/determinism/digests.txt` | The SHA-256 digests of the corpus outputs (SPEC §2.4). |
| `crates/folio-text/tests/golden/` | Shaped paragraphs, golden files of the shaping stage. |
| `crates/folio-syntax/tests/corpus/` | The parser's own corpus of `.fol` and `.fsty` files (round trips and incremental reparsing). |
| `fuzz/` | The `cargo fuzz` targets (nightly Rust), a workspace of its own; `fuzz/regressions/<target>/` keeps inputs that once failed. |
| `tests/pdf/`, `tests/office/` | The prototypes that verified the PDF and Office assumptions of SPEC §16.2, with their own READMEs. |

## What runs when

| When | Command | What it checks |
| --- | --- | --- |
| Before every push | `scripts/check.sh` | Build, every test (golden files, fuzz regressions and seeds on stable, determinism, PDF/UA-1), clippy, rustfmt, rustdoc, the wasm32 build of `folio-render`, MSRV, cargo-deny for both workspaces. |
| Before pushing a branch, and after each merge to main | `scripts/bench.sh` | The regression gate: instruction counts of the benchmarks against where the branch started; fails above 3%. Needs Valgrind and `gungraun-runner`. |
| On a schedule (nightly or weekly) | `scripts/fuzz.sh [seconds]` | The fuzz smoke job: each target for 60 s by default. Needs nightly Rust and `cargo-fuzz`. |
| Nightly, on the reference machine | `scripts/bench.sh --wall` | Wall-clock times of the M0 exit check (gated: 200 ms), the M1 exit check through `folio watch` (gated: 80 ms) and of the reference document against the targets of SPEC §14.1 (reported). |
| After changes to PDF output | `python tests/pdf/corpus.py` | veraPDF's PDF/UA-1 profile and text extraction (`tests/pdf/README.md`). |
| At each milestone close, and after moving a crate in `third_party/` to a new release | The aarch64 check (below) | The corpus gives the digests of `digests.txt` on aarch64 as on x86-64, and `cargo test -p folio-pdf --test colour` passes there and on x86-64 without AVX2 (SPEC §2.4). |

A schedule is up to the machine that runs it, for example a `cron` entry
that runs `scripts/fuzz.sh 600` and `scripts/bench.sh --wall` and mails
failures.

## Golden files

Expected output lives in files and is compared exactly, except page
images, which are compared within a tolerance (below). After a change
meant to alter output, run

```bash
scripts/bless.sh
```

which rewrites every golden file (layout dumps, page images, shaped
paragraphs and the determinism digests), removes layout files whose build
or page is gone, and prints what changed. Review that diff, and the
changed images, before committing it: a difference nobody meant is a bug. `FOLIO_BLESS=1` on a single golden test does the same for
that test.

A layout dump lists each page, then every fragment, glyph run and shape on
it, indented by nesting, with positions relative to the fragment that holds
it and lengths in points rounded to hundredths. A fragment names the source
it is the box of (`main.fol:12:1`), a run its font by PostScript name,
size, width, glyph count and text. So a diff reads as "this paragraph moved
3 pt down" or "this line now breaks after a different word".

A page image is what `folio-render` draws of a page's display list, at
72 dpi on white, as PNG: what a reader sees, so it catches what the
renderer gets wrong and what a dump cannot show. The renderer uses scalar
float code whose results IEEE 754 fixes, so a page should give the same
pixels on every machine; the golden test still allows each channel of a
pixel to differ by 2 levels of 255 (`golden::TOLERANCE`), and fails on
anything more. Blessing rewrites an image only when a pixel changed. On a
failure the test writes the image it got to `target/golden/<name>.png` and
says how many pixels differ. The same test also sends every page's
display list through the wire format and back.

## Determinism on another architecture

The determinism test compares the corpus outputs with digests recorded on
another machine, so it catches output that depends on the platform only
when it runs on one that differs. SVG figures are where that happened:
upstream crates called the platform's maths library, and an arrowhead in
`tests/corpus/figures` turned by a different last bit on aarch64 (#300).
They are now patched to call `libm` (`third_party/README.md`), as are the
crates that read PDF figures converted to SVG (#304; the figure
`tests/corpus/figures/assets/spectrum.pdf` turns its colours' last bits
into visible ones), and the corpus gives the same digests on x86-64 and
aarch64. The colour conversion and JPEG 2000 decoding of those figures
(`moxcms`, `hayro-jpeg2000`) also fused multiply-adds on some targets
only and picked SIMD paths by CPU at run time, so the same figure could
convert to other colours even on two x86-64 machines (#312); their
patched copies compute the same everywhere. `cargo test -p folio-pdf --test colour`
converts `crates/folio-pdf/tests/data/colour.pdf` and compares a digest
recorded on x86-64 with AVX2; `third_party/README.md` says how to run it
under QEMU on aarch64 and on x86-64 CPUs without AVX2 or SSE 4.1.

To check it on an aarch64 machine (Linux, or macOS on Apple Silicon), run
the determinism test there:

```bash
cargo test -p folio --test determinism
```

On x86-64 Linux, the corpus can be built for aarch64 and run under QEMU's
user-mode emulation instead. The test itself starts child processes, which
emulation runs only when binfmt_misc is set up for aarch64, so build the
corpus with the command line and compare the digests:

```bash
apt-get install qemu-user gcc-aarch64-linux-gnu libc6-dev-arm64-cross
rustup target add aarch64-unknown-linux-gnu
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    cargo build --release -p folio-cli --target aarch64-unknown-linux-gnu
for dir in tests/corpus/*/; do
    out=$(mktemp -d)
    cp -R "$dir" "$out/project"
    QEMU_LD_PREFIX=/usr/aarch64-linux-gnu qemu-aarch64 \
        target/aarch64-unknown-linux-gnu/release/folio build --root "$out/project" >/dev/null
    sha256sum "$out"/project/out/*.pdf
done
```

The command line names a project's outputs after its entry file
(`main.pdf`, `main-exam.pdf`), the digests file after the project
(`figures.pdf`, `exam-exam.pdf`); each digest must appear in
`crates/folio/tests/determinism/digests.txt`.

## The acceptance exam

`tests/corpus/acceptance` is the acceptance exam of SPEC §15: a four-page
A-level physics paper on mechanics, electricity and waves, written as a
teacher would write it, that the M1 exit criteria are judged on. It uses
inline and display maths with numbered, labelled equations and one
`$:tex …$` equation; quantities, units and numbers (`#qty`, with an
uncertainty and an angle) and a semantic vector; numbered questions
(headings) with parts (nested enumerations); references to equations,
figures and questions, with a supplement, and page references to a later
and an earlier page; footnotes on three pages; a bulleted list with a
marker; and two numbered figures: a force diagram with arrowheads as SVG
(`assets/sledge.svg`, its letters drawn as strokes, since Folio does not
set text in SVG yet) and an oscilloscope trace as PNG (`assets/trace.png`,
generated: a sine of period 4 and amplitude 2.5 divisions on a 10 × 8
grid). Its mark scheme comes with the exam component (M2).

It is checked as every corpus document is (layout dumps and page images,
PDF/UA-1, determinism, the command line), and its golden page images
were reviewed by hand once; a change to them is reviewed as a change to
what a teacher sees. The M1 exit check of `scripts/bench.sh --wall`
(`watch-edit`) previews it.

Project names in the corpus have no hyphen: the command-line test reads
the project from a digest's file name up to the first `-` or `.`.

## Adding a corpus document

1. Add a project directory under `tests/corpus/` with `folio.toml`,
   `main.fol` and its stylesheets, as the existing ones do.
2. Run `scripts/bless.sh` to write its layout golden files and its
   determinism digests, and read the dumps to check the layout is right.
3. Mention what it covers in `tests/pdf/README.md`, which lists the corpus.

## Fuzzing

`scripts/fuzz.sh` seeds each target from the corpora, builds the targets
with `cargo fuzz`, and runs each in turn. The targets' bodies are in
`crates/folio-testkit/src/fuzz.rs`:

| Target | Checks |
| --- | --- |
| `markup` | Parsing content markup is lossless. |
| `stylesheet` | Parsing a stylesheet is lossless. |
| `fmt` | Formatting content or a stylesheet (the first byte chooses) does not fail as `Unsound` on a file without syntax errors, and gives a file without syntax errors that parses to an equivalent tree and is formatted already. |
| `reparse` | After each edit of a sequence to a file, an incremental reparse equals a full parse, and node identities follow the edit. Its seeds include, for each corpus file, delimiters typed and deleted again, and the file made dense with syntax errors with a binding typed into it a character at a time. |
| `style` | Evaluating content and matching a stylesheet's selectors against it finishes without a panic, within small evaluation limits. |
| `nesting` | A script that nests content, arrays, dictionaries, closures and arguments by applying wrappers in loops compiles to PDF on a 2 MiB stack, or stops at the nesting limit, without a panic or a stack overflow. |
| `differential` | After each edit of a random sequence to a corpus project's content and stylesheets, applied in place through `World::edit_source`, every build has the outputs, the diagnostics and the span maps of its pages (spans resolved to byte ranges) of a world built afresh from the same texts. The edits insert markup, scripts, equations, labels and references, footnotes (one long enough to split across pages), page-number references and images into content, and rules with box properties, widows and orphans, page breaks, page sizes and per-page footnote numbering into stylesheets. It catches stale results in the memoised queries and in the shaping, line-break and page caches, which live outside salsa, among them the page cache's queue of carried notes and the layout passes that settle page references. |
| `document` | Evaluating content markup and building its document tree, with no stylesheet, finishes without a panic. |
| `compile` | A whole build of content with a stylesheet generated from the property registry (each three bytes a selector, a property and a value of its type, among them huge, zero, negative and non-finite lengths and numbers, and page sizes past PDF's limits) goes through pages and the PDF writer without a panic. |
| `manifest` | Reading a `folio.toml` finishes without a panic, and every build of a valid one compiles a small document. |
| `lockfile` | Reading a `folio.lock` beside a fixed manifest finishes without a panic, and a valid one compiles. |
| `wire` | Decoding a display list message of `folio-render`'s wire format finishes without a panic, and what decodes encodes to a message that decodes to the same. |
| `tex` | Reading the body of a LaTeX equation (`$:tex …$`) into the math tree finishes without a panic, within the nesting limit, and every error of the build points into the file. |

The `differential` harness also runs as a property test on stable:
`cargo test -p folio-testkit --test differential` checks 12 random
sequences of 6 edits beyond the seed corpus. `FOLIO_DIFFERENTIAL_CASES=<n>`
runs more, and a failure names its seed, which
`FOLIO_DIFFERENTIAL_SEED=<seed> FOLIO_DIFFERENTIAL_CASES=1` reruns alone.
`crates/folio/tests/incremental.rs` checks that results are reused: a
one-character edit in a paragraph does not re-run the cascade, and a
reflow confined to one page takes the other pages from the page cache.

When a target fails, libFuzzer writes the input to `fuzz/artifacts/<target>/`.
Reproduce it with `cargo +nightly fuzz run <target> <file>`, fix the bug,
and commit the input to `fuzz/regressions/<target>/` with the fix;
`cargo test -p folio-testkit --test fuzz` replays it from then on.

## Benchmarks and the reference document

`folio_testkit::refdoc` generates the reference document of SPEC §14.1
(100 A4 pages, 30 figures, 400 display equations, 20 tables) and smaller
documents in the same proportions, the same on every machine. Equations
and tables are stand-ins of about their size until math (M1) and tables
(M2) land; replace a stand-in when its feature does, and re-tune
`refdoc::CHARS_PER_PAGE` if `cargo test -p folio-testkit --test refdoc`
says the page count is off.

`refdoc::generate_variants` generates the same document with three
variants (`exam`, `markscheme` with a stylesheet overlay that sets it at
10pt, `teacher`) and an answer at the end of every page that only the last
two show (they run a little longer); the exam is `refdoc::generate`'s
document unchanged. `refdoc::Edit` holds the edits the benchmarks make:
a word in the middle, and the body's font size.

The instruction-count benchmarks (`cargo bench -p folio-testkit --bench compile`):

| Benchmark | What it runs |
| --- | --- |
| `compile_cold m0_exit` | The M0 exit check: the ten-page document from source to PDF in a fresh world. |
| `compile_cold exam`, `notes` | Corpus documents from source to PDF in a fresh world. |
| `recompile_after_edit m0_exit` | The ten-page document recompiled to PDF after a word in its middle is replaced by a longer one. |
| `preview_after_edit reference` | The preview path: the reference document laid out again to its pages, without emit, after a word in its middle is replaced by a longer one. |
| `check_after_edit reference` | The check path (`folio check`): the reference document checked again after the same one-word edit, every stage before Emit and the PDF writer's check, without writing the PDF. |
| `restyle reference` | The preview path after a stylesheet edit: the reference document's body font size changes from 11pt to 12pt. |
| `compile_variants one`, `three` | The ten-page document with three variants from source to PDF in a fresh world: the teacher's variant alone, and all three one after another. |
| `parse reference` | Parsing the reference document's markup. |

Callgrind counts only the benchmark's own thread. The benchmarks run with
one `rayon` thread (`RAYON_NUM_THREADS=1`), and with one thread
`folio-text` shapes on the calling thread instead of handing paragraphs to
its shaping pool, so every count includes all the shaping its compile does
(`crates/folio-text/tests/shaping_thread.rs` checks this). The preview
benchmarks go through one function, `preview`, which stops at the pages;
the check benchmarks call `folio::check`, which also runs the PDF writer's
check and gathers the diagnostics.

`folio-wall` (`scripts/bench.sh --wall`) prints three tables, each against
the limits of SPEC §14.1 and §15:

- scenarios, each run 20 times after a warm-up and judged by its 95th
  percentile: the M0 exit check (gated: 200 ms); the M1 exit check
  (`watch-edit`, gated: 80 ms), which serves the acceptance exam
  (below) with `folio watch`'s preview (`folio-preview`), watching a
  temporary copy on disk, writes an edit of a quantity on its second page
  to it and times until a websocket client has the display lists of the
  pages the edit changed; the same edit timed to the completed canvas
  (`watch-paint`, gated: 80 ms), until the viewer in Chromium, driven by
  `crates/folio-preview/tests/paint.cjs`, shows the edited page with every
  glyph drawn (some, at worst, from another quarter-pixel offset until the
  right one arrives); it fails if a frame with holes was shown, and runs
  only when Node.js can load Playwright and its Chromium, so a machine
  without them does not check this gate; the reference document's
  full compile, one-word edit to PDF, one-word edit to the pages (the
  preview of the M1 exit check: 80 ms), one-word edit checked without
  emit (reported only), stylesheet edit, and the ratio of
  three variants in parallel to the teacher's variant alone (all but the
  M0 exit check are release criteria and only reported);
- stages of the reference document's PDF build: the median and 95th
  percentile of applying the edit, `evaluate`, `document`, `style`,
  `text`, `lines`, `pages` and emit (with the build's checks), for a full
  compile, a one-word edit and a stylesheet edit, measured by calling the
  public queries in pipeline order;
- typing: one character every 100 ms into a paragraph in the middle of the
  reference document (40 by default, `--keystrokes N`), each timed from
  the edit to the pages, against the M1 exit check's 80 ms.

### Figures at milestone close

The reference machine of SPEC §14.1 is the project's `folio` cloud
environment. Each milestone's close records `scripts/bench.sh --wall` there:
two runs, back to back, on an otherwise idle container after
`target/release` was removed. A 95th percentile moved by up to 12%
between runs, and the typing session's, from 40 keystrokes, by a third (M1 close).

M1 close, 27 November 2026, at `7c53648`; 4 cores of an Intel Xeon at
2.10 GHz, 16 GB; the reference document had 97 pages. Run 1 / run 2:

| Scenario | Median | p95 | Target | Limit | Status |
| --- | --- | --- | --- | --- | --- |
| `m0-exit` | 27.8 / 30.2 ms | 33.1 / 37.2 ms | | 200 ms | ok |
| `reference-full` | 247.3 / 266.3 ms | 269.9 / 295.1 ms | 400 ms | 1,000 ms | ok |
| `reference-edit` | 191.6 / 194.8 ms | 222.6 / 221.3 ms | | 600 ms | ok |
| `preview-edit` | 71.6 / 75.2 ms | **81.5 / 86.1 ms** | | 80 ms | **over the limit** in both runs |
| `watch-edit` (M1 exit check) | 21.6 / 21.5 ms | 27.6 / 25.0 ms | | 80 ms | ok |
| `check-edit` | 85.2 / 85.5 ms | 97.9 / 92.2 ms | | | reported |
| `stylesheet-edit` | 224.8 / 230.6 ms | 241.3 / 266.1 ms | 400 ms | 1,000 ms | ok |
| three variants against one (p95) | | 1.38× / 1.30× | 1.3× | 2× | over the target, within the limit |
| typing, 40 characters, edit to pages | 66.1 / 70.7 ms | **82.4 / 110.2 ms** (max 89.7 / 113.3) | | 80 ms | **over the limit** in both runs |

Emit took 124 ms (median) of the 229 ms full build in run 1.
