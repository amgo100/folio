# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

Folio is a typesetting system, written in Rust, that compiles one semantic source into PDF, DOCX, PPTX and live presentations. Its primary use is teaching material: exams with mark schemes, notes, handouts and lesson slides from shared sources.

The repository is at **M0 "Skeleton"** (SPEC §15): `docs/SPEC.md` holds the design, and a Cargo workspace with the starting crate set of §2.2 exists, still without functionality.

## Build, lint and test

Rust edition 2024, MSRV 1.95 (`rust-version` in `Cargo.toml`; the `msrv` CI job and `dtolnay/rust-toolchain@1.95` in `.github/workflows/ci.yml` must move together). CI runs these, and all must pass:

```bash
cargo build --workspace --all-targets --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all --check
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
cargo deny check --all-features   # licences, advisories, bans, sources; config in deny.toml
```

- One test: `cargo test -p folio-component --test dependency_direction`. Run the binary with `cargo run -- --version`.
- `cargo deny` is not bundled with rustup: `cargo install cargo-deny --locked`.
- Crates live in `crates/` (`folio-syntax`, `folio-component`, `folio-core`, `folio-pdf`, `folio`). `folio-core` holds the stages that are not yet their own crates; split crates out of it as interfaces settle (§2.2).
- Lints are set once in `[workspace.lints]` in the root `Cargo.toml`; every crate has `[lints] workspace = true` and `#![forbid(unsafe_code)]` in each crate root (`lib.rs`, `main.rs`). `missing_docs` is on, so public items need doc comments.
- **Dependency direction**: `folio-component` must not depend (normally or at build time) on any other workspace crate except those in `ALLOWED` in `crates/folio-component/tests/dependency_direction.rs` (`folio-text`, `folio-draw` once they exist). The test fails otherwise; move shared types into `folio-component` instead of widening the list.
- New dependencies must pass `deny.toml`: permissive licences only, no unknown registries or git sources.

## Code derived from Typst

`folio-syntax` (from `typst-syntax`) and `folio-math` (from Typst's math layout) derive from Apache-2.0 code. When importing Typst code: copy from a tagged release, keep Typst's copyright line, put the attribution header shown in `NOTICE` at the top of every derived file, update that crate's entry in `NOTICE`, and refresh `third_party/typst/LICENSE` and `NOTICE` if the release differs from the one recorded. Folio's own licence is undecided (SPEC §16.1).

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
- **Incremental and deterministic** (§2.3, §2.4): every stage is a memoised query built on `salsa` (early cutoff, cancellation, durability, cycle handling for the page-number fixpoint). Page breaking is strictly forward; the flow state is split into a *layout part* (decides breaks, must be `Eq` with a stable hash) and a *furniture part* (page numbers, running marks). The same inputs and the same compiler version must give byte-identical output: no wall-clock time; fonts and components pinned via `folio.lock`; `folio.toml` pins the language edition (syntax and semantics only) and the compiler version (output).
- **Crates** (§2.2): a workspace of `folio-*` crates (`syntax`, `eval`, `model`, `style`, `component`, `text`, `fonts`, `math`, `draw`, `layout`, `anim`, `runtime`, `edit`, `render`, `pdf`, `ooxml`, `preview`, `lsp`, `node`) plus the `folio` lib/bin exposing `folio::World` and `folio::compile(world, target, variant)`. This is the target structure; development starts with fewer crates and splits them out as interfaces settle, except the `folio-component` boundary, which is fixed from the start. `folio-syntax` starts as a fork of `typst-syntax` and `folio-math` from Typst's math layout (both Apache-2.0); PDF goes through `krilla`; `folio-render` and `folio-runtime` stay free of tokio and threads so they can compile to WebAssembly. Folio Studio (TypeScript/Electron, §12.4) lives in the same repo and uses `folio-node` (napi-rs).
- **Small core, components** (§3): anything not every document needs is a component that reaches the core only through the extension points in §3.2. First-party components (`folio-component-exam`, `-multicol`, `-plot`, `-timer`, `-inputs`) depend only on `folio-component`, never on core internals, and they are versioned with the compiler, not frozen per edition. The test for core vs component is in §3.1. Layout containers (§3.3) are the most demanding extension point and must support multicol as a component.
- **Source formats** (§4–§5): `.fol` content markup with a scripting layer, `.fsty` stylesheets (a CSS-like hybrid: expressions only in values and render templates; rationale in Appendix A; parsed losslessly like content, with one expression grammar shared by scripts, stylesheet values and templates), `folio.toml` manifest. Content says what things are; stylesheets say how they look. Local overrides are tracked; strict mode forbids them.
- **Variants** (§6): one source yields several outputs (exam, markscheme, slides); numbering groups share counters resolved on the common tree, and a leak check guarantees hidden content does not reach outputs.
- **Editor-first** (§1.4, §12.4): source positions, span maps, node identity and edit transactions exist from the first version so the visual editor is a front end, not a retrofit.

## Constraints

- `#![forbid(unsafe_code)]` everywhere except `folio-node` and font memory-mapping (§14.2). Those two will declare their own `[lints]` table instead of inheriting the workspace one.
- Performance targets in §14.1 are release criteria (e.g. 5 ms for a typing edit's display list, warm full compile ≤ 400 ms p95); benchmark regressions are gated on instruction counts (`iai-callgrind`, fail above 3%).
- Testing (§14.2): golden files for layout, normalised OOXML and animation steps; leak tests on every variant; the `folio-component` conformance suite for every component; fuzzing of parser and style matcher.
- Component contracts (§3.5): pure, deterministic, memoisable, encapsulated, paint-only states, and total (a missing target mapping falls back, never fails a build).
