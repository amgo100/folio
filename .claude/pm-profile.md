# PM profile

The project-manager skill reads this file before every run on this repository. It wins over the profile saved on the board.

## board
https://claude.ai/artifact/9Po7tAqiim7gjwQoQANm5g

## defaultBranch
main

## audience
A teacher writing exams, notes and slides in Folio, today through the `folio` command line and AI coding agents that read its diagnostics; later Folio Studio.

## read
`CLAUDE.md` (all of it), `docs/SPEC.md` (§1.5 terminology, §2–§3, and the sections the issue names), `tests/README.md`.

## environment
Run the manager, and start every worker, in the cloud environment **folio** (`env_01Mvv8bmCsJLhRE8FAMwA8Gy`): pass it as `environment_id` to `create_session`. Its setup script installs everything in `setup` below, including veraPDF on `PATH`. The **Default** environment has none of it.

## setup
In the **folio** environment these are already installed. Elsewhere, run them first:
- `rustup toolchain install 1.95 --profile minimal`
- `cargo install cargo-deny --locked`
- For benchmarks: Valgrind and `cargo install gungraun-runner --version <the gungraun version in Cargo.lock> --locked`.
- For fuzzing: `rustup toolchain install nightly --profile minimal` and `cargo install cargo-fuzz --locked`.
- For the PDF/UA check: Java, Poppler's `pdftotext`, and veraPDF 1.28.2 (`org.verapdf.apps:greenfield-apps` from Maven Central, run through `org.verapdf.apps.GreenfieldCliWrapper`).

## gate
- commands, in order, on the final head commit:
  1. `scripts/check.sh`, which must end with "all checks passed" (iterate with `scripts/check.sh --fast` and narrow `cargo test -p <crate> --test <name>`; the full script before every push).
  2. `scripts/bench.sh` when the change may affect performance (fails above 3%); `scripts/bench.sh --wall` for preview and typing figures.
  3. `python tests/pdf/corpus.py` after changes to `folio-pdf` or to what layout draws (veraPDF is on `PATH` in the folio environment).
  4. `scripts/fuzz.sh 60` for changes to the parser, the style matcher or a fuzz target.
- docsOnly: none. Spec-only and documentation changes run the full gate too (CLAUDE.md).
- codeReview: yes. CLAUDE.md requires `/code-review` on the branch's diff against its base before the PR is opened and before every later push.
- ciRuns: no. There is no hosted CI. The PR body says which gate commands passed on which commit.

## labels
- priority: none.
- area: `area: <name>` (`core`, `eval`, `style`, `text`, `layout`, `cli`, `infra`, `pdf`, `ooxml`, `components`, …).
- noDecisions: none.
- spec: `question` (a decision for the user from the start).
- tracking: GitHub sub-issues under an issue labelled `epic`; milestones are named `M<n> — <name>` (M0 = 1, M1 = 2, M2 = 3, M3 = 4).
- status: none.
- issue type: every issue filed by the manager or a worker gets a GitHub issue type:
  - `Bug`: something that behaves wrongly or contradicts the spec, including test or benchmark tooling that gives wrong results;
  - `Feature`: new functionality a user (teacher, agent or developer using the library) can use;
  - `Task`: everything else, such as groundwork, refactors, performance work, tests, docs and spec decisions.
  Add the matching `bug` or `enhancement` label and an `area:` label as well. The issue type does not replace the labels.

## hotSpots
- `Cargo.toml`, `Cargo.lock`, crate `Cargo.toml` files, `fuzz/Cargo.lock`: one dependency change at a time; regenerate lockfiles with cargo.
- `fuzz/Cargo.toml`, `crates/folio-testkit/src/fuzz.rs`: every fuzz target is listed in both.
- `docs/SPEC.md`: owned per section (§n.m). Two unfinished items never edit the same section.
- `CLAUDE.md`: its project-status paragraph is one physical line, so any two branches that edit it conflict. Workers do not edit it. They put the proposed wording in the PR body, and one consolidating PR applies it.
- `tests/golden/layout/`, `crates/folio-text/tests/golden/`, `crates/folio/tests/determinism/digests.txt`: one item that changes output at a time. Every other item must leave them unchanged and reports `blocked` if they move.
- `crates/folio-testkit/src/refdoc.rs`: changing it changes every benchmark's workload.
- `crates/folio-core/src/style/tree.rs`, `style/rules.rs`: the styled tree, matching and the cascade.
- `crates/folio-core/src/text.rs`: collection, pairing by location, shaping; touched by most text, layout and diagnostics work.
- `crates/folio-core/src/layout/` (`mod.rs`, `page.rs`, `flow.rs`, `cache.rs`): the layout stage.
- `crates/folio/src/lib.rs`, `crates/folio/src/pipeline.rs`: public API and the compile root.
- `crates/folio/src/cli/`: the command line.
- `crates/folio-text/src/cache.rs`: shaping pool and cache.
- `crates/folio-component/src/fragment.rs`: the `FragmentItem` enum is matched exhaustively in tests (`tests/check.rs`, `tests/pages.rs`, `folio-testkit/src/golden.rs`), so a new variant touches those files too.
- `tests/README.md`: separate sections (fuzzing, benchmarks) can be owned by different items.

## decisions
- The language's semantics and anything SPEC §16.1 lists as open.
- Spec text that changes normative behaviour, beyond recording an issue's own stated scope.
- The format of `folio.toml`, `folio.lock`, `channel.toml` and the JSON diagnostics.
- The `folio-component` interface beyond adding a variant (components are versioned with the compiler, but first-party components build against it).
- Accepting a benchmark regression above the 3% gate.

## actions
none.

## sensitive
- Secrets and private signing keys (release signing, SPEC §14.3); test keys are generated by the test and never committed.
- The user's personal details.

## records
The `docs/SPEC.md` section the answer changes (open points stay in §16.1), plus the PR body.

## stacking
Never give a PR a base other than `main`. A PR stacked on another branch merged into that branch after it had landed (#242), and a stacked PR emptied by its base's merge was auto-merged and closed its issue (#264). Instead, cut the branch from the predecessor's branch, open the PR against `main`, and start its body with "Depends on #N — merge that first". Merge `main` in once the predecessor lands, and only then open or update the PR; never push a merge that leaves the branch with no commits of its own while the PR is open.
