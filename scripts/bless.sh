#!/usr/bin/env bash
# Rewrites every golden file from the current output (SPEC §14.2): the
# layout fragment dumps and page images in tests/golden, the shaped
# paragraphs of folio-text, the math goldens of folio-math and the
# determinism digests. Run it after a
# change meant to alter output, then review the diff it prints, and the
# changed images, before committing; a change nobody meant is a bug.
# Usage: scripts/bless.sh
set -euo pipefail
cd "$(dirname "$0")/.."

export FOLIO_BLESS=1
cargo test --locked -p folio-testkit --test golden
cargo test --locked -p folio-text --test golden
cargo test --locked -p folio-math --test golden
cargo test --locked -p folio --test determinism

printf '\n==> changed golden files\n'
git status --short -- tests/golden crates/folio-text/tests/golden crates/folio-math/tests/golden crates/folio/tests/determinism
git diff --stat -- tests/golden crates/folio-text/tests/golden crates/folio-math/tests/golden crates/folio/tests/determinism
printf '\nPage images are PNG files: look at the changed ones before committing.\n'
