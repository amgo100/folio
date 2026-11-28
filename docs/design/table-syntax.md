# Table syntax: options

Status: proposal for decision D-62-1 (issue #62). Nothing here is implemented beyond option A, which is what the content model allows today.

The layout engine of SPEC §7.4 (the `grid` element, track sizing, placement, page breaking, repeated headers, footnotes in cells, PDF table tags) does not depend on how cells are written. It sees a `grid` element whose body is a sequence of cells, each with `colspan`, `rowspan` and optional `column` and `row`. Every option below produces that same tree, so the choice is only about the surface syntax.

The user's requirements (D-62-1):

- Writing larger cells must not be cumbersome.
- Large tables must stay manageable.
- Tables from external data must be easy.
- Column and row spans must be possible.
- Sparse tables, with many empty cells, must be easy.

## What the language has today

- Content markup is Typst-like. `[ … ]` in markup is plain text: brackets only delimit content after a function call. That is why the §7.4 example (`[Quantity] [Symbol]`) does not work.
- A call takes named fields and one trailing content block, its body: `#figure(caption: [..])[..]`. There are no variadic content arguments (`#f[a][b][c]`).
- Lists (`-`), enumerations (`+`), headings, maths, raw blocks and scripting (`#let`, `#for`, `#if`) exist.
- `csv()` and `json()` are planned in #60. Today a `#for` loop over an array already produces cells: `#for (l, t) in rows [ #cell[#l] #cell[#t] ]`.

## The five examples

Every option is shown on the same five tables:

- (a) A 3-column physics data table with a header row and units.
- (b) A mark-scheme table whose answer cells hold several paragraphs, a list or a formula.
- (c) Spans across columns and rows.
- (d) A sparse table, with many empty cells.
- (e) A table generated from external data, header included.

---

## Option A: cells as calls (what exists today)

Each cell is a `#cell[..]` call, or any block of the body. Cells flow into rows by the number of columns.

Borrowed from: Typst's `table(..)`/`table.cell(..)`, and LaTeX's flow of `&`-separated cells, but with explicit cell boundaries.

**(a)**
```
#grid(column-tracks: (auto, 1fr, 1fr), header-rows: 1)[
  #cell[Length $l$ / m] #cell[Period $T$ / s] #cell[$T^2$ / s²]
  #cell[0.200] #cell[0.897] #cell[0.805]
  #cell[0.250] #cell[1.003] #cell[1.006]
]
```

**(b)**
```
#grid(class: "markscheme", column-tracks: (auto, 1fr, auto), header-rows: 1)[
  #cell[Question] #cell[Answer] #cell[Marks]
  #cell[3(a)] #cell[
    Use of $v = u + a t$.

    $a = 18 / 6.0 = qty(3.0, "m s^-2")$
  ] #cell[2]
  #cell[3(b)] #cell[
    Any two of:
    - air resistance is ignored;
    - $g$ is uniform;
    - the ball starts from rest.
  ] #cell[2]
]
```

**(c)**
```
#grid(column-tracks: 3, header-rows: 1)[
  #cell[Kind] #cell[Source] #cell[Remedy]
  #cell(rowspan: 2)[Random] #cell[Reaction time] #cell[Time ten swings]
  #cell[Amplitude varies] #cell[Release from a mark]
  #cell(colspan: 3)[Systematic errors are discussed below.]
]
```

**(d)** Empty cells must be written, or skipped with explicit places:
```
#grid(column-tracks: 6, header-rows: 1)[
  #cell[] #cell[Mon] #cell[Tue] #cell[Wed] #cell[Thu] #cell[Fri]
  #cell[9:00] #cell[] #cell[Physics] #cell[] #cell[] #cell[Lab]
  #cell(row: 3, column: 1)[10:00] #cell(row: 3, column: 4)[Maths]
]
```

**(e)**
```
#let data = csv("results.csv")
#grid(column-tracks: data.first().len(), header-rows: 1)[
  #for row in data { for value in row [#cell[#value]] }
]
```

- **Large tables (30+ rows):** one row per line works, but `#cell[ ]` is a third of the characters.
- **`folio fmt`:** keeps each cell where it is. It cannot align columns, because it does not know where rows end.
- **Parser:** no change.
- **AI agents:** reliable to write, since every boundary is explicit. They tend to forget that a block of the body is a cell, and write a stray paragraph that shifts every later cell by one.
- **DOCX/PPTX:** direct. Each cell becomes a `w:tc`, and spans become `w:gridSpan` and `w:vMerge`.
- **Verdict:** complete and unambiguous, but verbose for simple tables. This is the user's objection.

---

## Option B: pipe rows inside the grid body, with `#cell` for rich cells

Inside a `grid`'s body, a line that starts with `|` is a row. `|` separates single-line cells; `||` writes an empty cell. A rich cell, with paragraphs, lists or a display, is written `#cell[..]` in the row's place, and its content block may run over several lines. Spans use a prefix: `2+|` spans two columns and `.2+|` two rows. Explicit `#cell(colspan: …)` works too.

Borrowed from:
- Markdown pipe tables (rows as lines);
- AsciiDoc tables (`|===` bodies, `2+|` spans, `a|` cells that hold blocks);
- the explicit cells of option A.

**(a)**
```
#grid(column-tracks: (auto, 1fr, 1fr), header-rows: 1)[
  | Length $l$ / m | Period $T$ / s | $T^2$ / s² |
  | 0.200          | 0.897          | 0.805      |
  | 0.250          | 1.003          | 1.006      |
]
```

**(b)**
```
#grid(class: "markscheme", column-tracks: (auto, 1fr, auto), header-rows: 1)[
  | Question | Answer | Marks |
  | 3(a) | #cell[
      Use of $v = u + a t$.

      $a = 18 / 6.0 = qty(3.0, "m s^-2")$
    ] | 2 |
  | 3(b) | #cell[
      Any two of:
      - air resistance is ignored;
      - $g$ is uniform;
      - the ball starts from rest.
    ] | 2 |
]
```

**(c)**
```
#grid(column-tracks: 3, header-rows: 1)[
  | Kind         | Source           | Remedy              |
  .2+| Random    | Reaction time    | Time ten swings     |
                 | Amplitude varies | Release from a mark |
  3+| Systematic errors are discussed below.             |
]
```

**(d)**
```
#grid(column-tracks: 6, header-rows: 1)[
  |       | Mon | Tue     | Wed | Thu | Fri |
  | 9:00  |     | Physics |     |     | Lab |
  | 10:00 |     |         | Maths |   |     |
]
```

**(e)** As in A: a loop yields `#cell`s. Rows of pipes are markup, not code, so data stays in code:
```
#let data = csv("results.csv")
#grid(column-tracks: data.first().len(), header-rows: 1)[
  #for row in data { for value in row [#cell[#value]] }
]
```

- **Large tables:** compact, and readable as a table in source, which is the main gain.
- **`folio fmt`:** aligns the pipes of rows whose cells are single-line, as Markdown formatters do, and indents `#cell[..]` bodies. It needs care with East Asian widths and maths, and leaves a row unaligned when a cell is too wide.
- **Parser:** a change to folio-syntax. It needs:
  - a table-row mode in markup inside a `grid` body (or any body, read as rows only by `grid`);
  - the `2+|` and `.2+|` span prefixes;
  - `\|` to escape a pipe in text.

  The cleaner variant parses `| … |` rows anywhere as a generic row node, which `grid` turns into cells and anywhere else is an error with a hint.
- **AI agents:** they write pipe tables very readily, but expect Markdown semantics. Common mistakes:
  - adding a `|---|` separator line (could be accepted and ignored, or read as "the rows above are header rows");
  - forgetting to escape `|` in maths (`$|x|$`; maths could be exempt);
  - miscounting cells in sparse rows.
- **DOCX/PPTX:** as in A; the rows give the same cells.
- **Verdict:** fits the common case (simple data tables), keeps rich cells possible, and needs a parser change.

---

## Option C: rows as nested lists (list tables)

Inside a `grid`'s body, a list is read as rows: each item is a row, and each item of its inner list is a cell. Cells are ordinary list items, so they hold paragraphs, lists and displays with no new syntax. Spans and places use `#cell(..)` as the inner item's content, or an attribute.

Borrowed from: reStructuredText's `list-table` and Pandoc's list tables.

**(a)**
```
#grid(column-tracks: (auto, 1fr, 1fr), header-rows: 1)[
  - - Length $l$ / m
    - Period $T$ / s
    - $T^2$ / s²
  - - 0.200
    - 0.897
    - 0.805
]
```

**(b)**
```
#grid(class: "markscheme", column-tracks: (auto, 1fr, auto), header-rows: 1)[
  - - Question
    - Answer
    - Marks
  - - 3(a)
    - Use of $v = u + a t$.

      $a = 18 / 6.0 = qty(3.0, "m s^-2")$
    - 2
  - - 3(b)
    - Any two of:
      - air resistance is ignored;
      - $g$ is uniform;
    - 2
]
```

**(c)**
```
  - - #cell(rowspan: 2)[Random]
    - Reaction time
    - Time ten swings
  - - Amplitude varies
    - Release from a mark
```

**(d)** An empty item is `-` alone on its line (today the parser may need to accept an empty list item).

**(e)** As in A, with a loop.

- **Large tables:** three to four times as many lines as B. Very diff-friendly, hard to scan as a table.
- **`folio fmt`:** list indentation is already canonical; nothing new.
- **Parser:** none, or a small one (empty list items). The model and text stages read a list in a `grid` as rows.
- **AI agents:** indentation slips (a cell's second paragraph not indented enough becomes a new row), and they confuse a nested list inside a cell (b) with the row/cell levels.
- **DOCX/PPTX:** direct.
- **Verdict:** best for tables of rich cells (mark schemes), poor for data tables.

---

## Option D: data-first tables (rows as values)

The rows are code values: `rows:` takes an array of arrays of content, or data from `csv()`/`json()` directly. `header:` takes a row, or `auto` for the data's first row. Cells are content values (`[..]`), with `cell(..)` for spans.

Borrowed from: Typst's `table(..cells)` (cells as arguments), LaTeX's `tabular` rows, and spreadsheet and dataframe APIs.

**(a)**
```
#grid(column-tracks: (auto, 1fr, 1fr), header: ([Length $l$ / m], [Period $T$ / s], [$T^2$ / s²]), rows: (
  ([0.200], [0.897], [0.805]),
  ([0.250], [1.003], [1.006]),
))
```

**(b)** Rich cells are content blocks inside an array: `([3(a)], [Use of … \n\n …], [2])`. Long cells become awkward inside parentheses and commas.

**(c)** `(cell(rowspan: 2)[Random], [Reaction time], [Time ten swings])`, then the next row omits the covered cell.

**(d)** `([9:00], none, [Physics], none, none, [Lab])`.

**(e)** The strongest case:
```
#grid(rows: csv("results.csv"), header: auto, column-tracks: (auto, 1fr, 1fr))
```

- **Large tables:** compact for data, noisy for text, with brackets, commas and parentheses.
- **`folio fmt`:** formats it as code; it could align columns of arrays.
- **Parser:** none. Eval needs fields that take arrays of content, and `grid` gains `rows`/`header` fields (a `folio-component` schema change for the field types).
- **AI agents:** good at it (JSON-like), but they mismatch brackets in long rich cells.
- **DOCX/PPTX:** direct.
- **Verdict:** ideal for external data, unpleasant for prose cells.

---

## Comparison

| | A cells | B pipe rows | C list rows | D data |
| --- | --- | --- | --- | --- |
| Simple data table (a) | verbose | best | verbose | good |
| Rich cells (b) | good | good (`#cell`) | best | poor |
| Spans (c) | good | good (`2+\|`) | fair | fair |
| Sparse (d) | fair (`#cell[]`) | best (`\|\|`) | fair | good (`none`) |
| External data (e) | good (loop) | good (loop) | good (loop) | best |
| 30+ rows | long lines | readable | very long | readable |
| Parser change | no | yes | small | no (schema yes) |
| `folio fmt` alignment | no | yes | n/a | possible |

## Recommendation

**B, with A and D's data path kept, as layers over one model.**

1. **Cells (A)** stay the model, and the escape hatch: every cell can be written `#cell(..)[..]`, so rich cells, spans and explicit places always work. This is the engine's input and what loops produce.
2. **Pipe rows (B)** become the everyday syntax for tables. They give the most readable source for the most common case: data and specification tables, sparse tables and 30-row tables. Rich cells drop into a row as `#cell[..]`. `fmt` can align pipe rows, which keeps large tables tidy, and agents already write the shape.
3. **Data (D-lite).** A `rows:` field that takes an array of rows, or a `csv()`/`json()` value, with `header: auto`, gives external data a one-liner. Loops remain for anything more selective. This can follow #60 without blocking it.

C is not recommended as a general syntax: it makes data tables three times longer. Its strength, rich cells, is covered by `#cell[..]` blocks in B.

**Plan.**
- Step 1 (this PR): the engine, with option A as it is.
- Step 2: the parser change for pipe rows (folio-syntax), with `fmt` alignment, as its own issue.
- Step 3: `rows:`/`header:` once `csv()`/`json()` land (#60).

Points for the user to settle with B:

- Whether a `|---|` line marks header rows, as in Markdown, or `header-rows:` stays the only way.
- Whether pipes inside maths need escaping.
- The span prefixes `2+|` and `.2+|` (AsciiDoc's), or only `#cell(colspan: 2)[..]`.
