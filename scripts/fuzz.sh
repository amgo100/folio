#!/usr/bin/env bash
# The fuzz smoke job (SPEC §14.2): runs every fuzz target in fuzz/ for a
# while, starting from the seed corpus. Usage: scripts/fuzz.sh [seconds]
#   seconds  how long to run each target (default 60)
# Needs nightly Rust and cargo-fuzz:
#   rustup toolchain install nightly --profile minimal
#   cargo install cargo-fuzz --locked
# Inputs found are kept in fuzz/corpus (not committed), so later runs go
# on from them. A failing input is written to fuzz/artifacts/<target>/;
# reproduce it with `cargo +nightly fuzz run <target> <file>`, fix the bug,
# and commit the input to fuzz/regressions/<target>/, where `cargo test`
# replays it.
set -euo pipefail
cd "$(dirname "$0")/.."

seconds=${1:-60}
if ! [[ "$seconds" =~ ^[0-9]+$ ]] || (( seconds == 0 )); then
    echo "usage: scripts/fuzz.sh [seconds]" >&2
    exit 2
fi
if ! rustup run nightly rustc --version >/dev/null 2>&1; then
    echo "nightly Rust is not installed: rustup toolchain install nightly --profile minimal" >&2
    exit 1
fi
if ! cargo fuzz --version >/dev/null 2>&1; then
    echo "cargo-fuzz is not installed: cargo install cargo-fuzz --locked" >&2
    exit 1
fi

step() { printf '\n==> %s\n' "$*"; }

step "seeds"
cargo run --locked --quiet -p folio-testkit --bin folio-fuzz-seeds -- fuzz/corpus

step "build"
cargo +nightly fuzz build -O

failed=()
for target in $(cargo +nightly fuzz list); do
    step "$target for ${seconds}s"
    # -timeout: an input that takes longer than 10 s counts as a hang.
    if ! cargo +nightly fuzz run -O "$target" "fuzz/corpus/$target" -- \
        -max_total_time="$seconds" -timeout=10 -rss_limit_mb=4096 -print_final_stats=1; then
        failed+=("$target")
    fi
done

if (( ${#failed[@]} > 0 )); then
    printf '\nfailed: %s; the inputs are in fuzz/artifacts/\n' "${failed[*]}" >&2
    exit 1
fi
step "no failures"
