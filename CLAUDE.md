# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

Folio is a typesetting system, to be written in Rust, that compiles one semantic source into PDF, DOCX, PPTX and live presentations. Its primary use is teaching material: exams with mark schemes, notes, handouts and lesson slides from shared sources.

The repository is at the **specification stage**: it contains only documentation (`README.md`, `docs/SPEC.md` and this file). There is no code, Cargo workspace, build, lint or test setup yet. Implementation follows the roadmap in SPEC §15 (M0 "Skeleton" first). When code is added, update this file with the real commands.

## Pull requests

Every pull request is reviewed with the `/code-review` skill (which may spread the review over reviewer subagents, depending on effort level):

- Before opening a PR, and before every later push to one, run `/code-review` on the branch's diff against `main`. Fix confirmed findings, or say in the PR why one does not apply, before pushing.
- When asked to review or check someone else's PR, use `/code-review <PR number>` rather than reviewing by hand; add `--comment` only when asked to post the findings on GitHub.
- This applies to spec-only and documentation changes too.

## Working on the specification

`docs/SPEC.md` is the single source of truth for the design. When editing it:

- Sections are cross-referenced by number (`§7.3`, `§10.5`) throughout, and the requirements table in §1.2 maps R1–R17 to sections. Renumbering or moving a section means updating every reference to it, including the §1.2 table, the roadmap (§15) and the assumptions table (§16.2).
- Terms in the §1.5 terminology table (element, component, variant, numbering group, common tree, flow state, live region, fragment, span map…) are used with those exact meanings; keep new text consistent with them, and add new terms there.
- Undecided points go in §16.1 (open questions) or §16.2 (assumptions with a verification method), not as hedges inside the normative sections.
- The spec uses British spelling (memoised, colour, licence, behaviour) and short declarative sentences.
- Diagrams are Mermaid blocks followed by a one-line italic caption.

## Architecture (as specified)

Read SPEC §2–§3 before writing any code; the key ideas that span many sections:

- **Pipeline** (§2.1): Parse (lossless, error-tolerant CST) → Evaluate (shared; only code that reads `variant` re-runs per variant) → then per variant and target: Style → Select → Sequence → Resolve → Step styling → Layout → Emit. The pipeline forks after Resolve and Step styling: PDF, live and preview go through Folio's own layout engine; DOCX and PPTX are generated from the resolved, step-styled tree (using Folio layout only for measurement).
- **Incremental and deterministic** (§2.3, §2.4): every stage is a memoised query (salsa/comemo style). Page breaking is strictly forward; the flow state is split into a *layout part* (decides breaks, must be `Eq` with a stable hash) and a *furniture part* (page numbers, running marks). Same inputs must give byte-identical output: no wall-clock time, fonts and components pinned via `folio.lock` and the language edition in `folio.toml`.
- **Crates** (§2.2): a workspace of `folio-*` crates (`syntax`, `eval`, `model`, `style`, `component`, `text`, `fonts`, `math`, `draw`, `layout`, `anim`, `runtime`, `edit`, `render`, `pdf`, `ooxml`, `preview`, `lsp`, `node`) plus the `folio` lib/bin exposing `folio::World` and `folio::compile(world, target, variant)`. Folio Studio (TypeScript/Electron, §12.4) lives in the same repo and uses `folio-node` (napi-rs).
- **Small core, components** (§3): anything not every document needs is a component that reaches the core only through the extension points in §3.2. First-party components (`folio-component-exam`, `-multicol`, `-plot`, `-timer`, `-inputs`) depend only on `folio-component`, never on core internals, and their behaviour is frozen per edition. The test for core vs component is in §3.1. Layout containers (§3.3) are the most demanding extension point and must support multicol as a component.
- **Source formats** (§4–§5): `.fol` content markup with a scripting layer, `.fsty` stylesheets (a CSS-like hybrid: expressions only in values and render templates; rationale in Appendix A), `folio.toml` manifest. Content says what things are; stylesheets say how they look. Local overrides are tracked; strict mode forbids them.
- **Variants** (§6): one source yields several outputs (exam, markscheme, slides); numbering groups share counters resolved on the common tree, and a leak check guarantees hidden content does not reach outputs.
- **Editor-first** (§1.4, §12.4): source positions, span maps, node identity and edit transactions exist from the first version so the visual editor is a front end, not a retrofit.

## Constraints to honour once code exists

- `#![forbid(unsafe_code)]` everywhere except `folio-node` and font memory-mapping (§14.2).
- Performance targets in §14.1 are release criteria (e.g. 5 ms for a typing edit's display list, warm full compile ≤ 400 ms p95); benchmark regressions are gated on instruction counts (`iai-callgrind`, fail above 3%).
- Testing (§14.2): golden files for layout, normalised OOXML and animation steps; leak tests on every variant; the `folio-component` conformance suite for every component; fuzzing of parser and style matcher.
- Component contracts (§3.5): pure, deterministic, memoisable, encapsulated, paint-only states, and total (a missing target mapping falls back, never fails a build).
