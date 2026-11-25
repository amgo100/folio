# Tests

How Folio is tested (SPEC §14.1, §14.2): what lives where, what runs when,
and how to update expected output. There is no hosted CI; every check runs
from the scripts in `scripts/`.

## Layout

| Path | What it holds |
| --- | --- |
| `tests/corpus/<project>/` | The test corpus: whole projects (`folio.toml`, content, stylesheets), each covering features a real document uses. Every build of every project goes through the golden, PDF, determinism and command-line tests. |
| `tests/golden/layout/` | Layout golden files: `<project>-<variant>.<target>.txt` for each corpus build that goes through layout, a text dump of its pages and fragments. Page images join them when the rasteriser lands (M1). |
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
| Before every push | `scripts/check.sh` | Build, every test (golden files, fuzz regressions and seeds on stable, determinism, PDF/UA-1), clippy, rustfmt, rustdoc, MSRV, cargo-deny for both workspaces. |
| Before pushing a branch, and after each merge to main | `scripts/bench.sh` | The regression gate: instruction counts of the benchmarks against where the branch started; fails above 3%. Needs Valgrind and `gungraun-runner`. |
| On a schedule (nightly or weekly) | `scripts/fuzz.sh [seconds]` | The fuzz smoke job: each target for 60 s by default. Needs nightly Rust and `cargo-fuzz`. |
| Nightly, on the reference machine | `scripts/bench.sh --wall` | Wall-clock times of the M0 exit check (gated: 200 ms) and of the reference document against the targets of SPEC §14.1 (reported). |
| After changes to PDF output | `python tests/pdf/corpus.py` | veraPDF's PDF/UA-1 profile and text extraction (`tests/pdf/README.md`). |

A schedule is up to the machine that runs it, for example a `cron` entry
that runs `scripts/fuzz.sh 600` and `scripts/bench.sh --wall` and mails
failures.

## Golden files

Expected output lives in files and is compared exactly. After a change
meant to alter output, run

```bash
scripts/bless.sh
```

which rewrites every golden file (layout dumps, shaped paragraphs and the
determinism digests), removes layout files whose build is gone, and prints
what changed. Review that diff before committing it: a difference nobody
meant is a bug. `FOLIO_BLESS=1` on a single golden test does the same for
that test.

A layout dump lists each page, then every fragment, glyph run and shape on
it, indented by nesting, with positions relative to the fragment that holds
it and lengths in points rounded to hundredths. A fragment names the source
it is the box of (`main.fol:12:1`), a run its font by PostScript name,
size, width, glyph count and text. So a diff reads as "this paragraph moved
3 pt down" or "this line now breaks after a different word".

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
| `reparse` | An incremental reparse after an edit equals a full parse, and node identities follow the edit. |
| `style` | Evaluating content and matching a stylesheet's selectors against it finishes without a panic, within small evaluation limits. |
| `nesting` | A script that nests content, arrays, dictionaries, closures and arguments by applying wrappers in loops compiles to PDF on a 2 MiB stack, or stops at the nesting limit, without a panic or a stack overflow. |
| `differential` | After each edit of a random sequence to a corpus project's content and stylesheets, applied in place through `World::edit_source`, every build has the outputs, the diagnostics and the span maps of its pages (spans resolved to byte ranges) of a world built afresh from the same texts. It catches stale results in the memoised queries and in the shaping, line-break and page caches, which live outside salsa. |

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
| `restyle reference` | The preview path after a stylesheet edit: the reference document's body font size changes from 11pt to 12pt. |
| `compile_variants one`, `three` | The ten-page document with three variants from source to PDF in a fresh world: the teacher's variant alone, and all three one after another. |
| `parse reference` | Parsing the reference document's markup. |

Callgrind counts only the benchmark's own thread. Paragraphs that
`folio-text` shapes several at a time run on its shaping thread, so a full
compile's and a stylesheet edit's counts leave most shaping out; the
wall-clock times include it. The preview benchmarks go through one
function, `preview`, to point at a query that stops at the pages once
there is one.

`folio-wall` (`scripts/bench.sh --wall`) prints three tables, each against
the limits of SPEC §14.1 and §15:

- scenarios, each run 20 times after a warm-up and judged by its 95th
  percentile: the M0 exit check (gated: 200 ms); the reference document's
  full compile, one-word edit to PDF, one-word edit to the pages (the
  preview of the M1 exit check: 80 ms), stylesheet edit, and the ratio of
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
