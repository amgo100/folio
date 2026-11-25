#!/usr/bin/env bash
# Rewrites every golden file from the current output (SPEC §14.2): the
# layout fragment dumps in tests/golden, the shaped paragraphs of
# folio-text and the determinism digests. Run it after a change meant to
# alter output, then review the diff it prints before committing; a change
# nobody meant is a bug. Usage: scripts/bless.sh
set -euo pipefail
cd "$(dirname "$0")/.."

export FOLIO_BLESS=1
cargo test --locked -p folio-testkit --test golden
cargo test --locked -p folio-text --test golden
cargo test --locked -p folio --test determinism

printf '\n==> changed golden files\n'
git status --short -- tests/golden crates/folio-text/tests/golden crates/folio/tests/determinism
git diff --stat -- tests/golden crates/folio-text/tests/golden crates/folio/tests/determinism
