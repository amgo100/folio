#!/usr/bin/env bash
# Benchmarks (SPEC §14.1). Usage:
#   scripts/bench.sh [--base <rev>]  the regression gate: instruction counts
#                                    of this checkout against <rev>
#   scripts/bench.sh --wall          wall-clock times against the limits
#
# The gate builds the benchmarks of crates/folio-testkit/benches at <rev>
# (by default where this branch left main: the merge base with origin/main,
# or the commit before HEAD when HEAD is on main), records their counts as
# a baseline, then runs them here and fails when one executes more than 3%
# more instructions. Run it before pushing a branch, and after each merge
# to main. It needs Valgrind and gungraun-runner of the version in
# Cargo.lock:
#   cargo install gungraun-runner --version <version> --locked
#
# --wall times the M0 exit check and the reference document with an
# optimised build and fails when the M0 exit check exceeds 200 ms (p95).
# Its numbers mean something on the reference machine of SPEC §14.1; run it
# there nightly.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$(pwd)

step() { printf '\n==> %s\n' "$*"; }

if [[ "${1:-}" == "--wall" ]]; then
    shift
    exec cargo run --locked --release -p folio-testkit --bin folio-wall -- --check "$@"
fi

base=""
if [[ "${1:-}" == "--base" ]]; then
    base=${2:?"--base needs a revision"}
    shift 2
fi
if (( $# > 0 )); then
    echo "usage: scripts/bench.sh [--base <rev>] | --wall" >&2
    exit 2
fi

if ! command -v valgrind >/dev/null; then
    echo "Valgrind is not installed" >&2
    exit 1
fi
version=$(sed -n '/^name = "gungraun"$/{n;s/^version = "\(.*\)"/\1/p;}' Cargo.lock)
if ! gungraun-runner --version 2>/dev/null | grep -qF "$version"; then
    echo "gungraun-runner $version is not installed: cargo install gungraun-runner --version $version --locked" >&2
    exit 1
fi

if [[ -z "$base" ]]; then
    main=origin/main
    git rev-parse --verify -q "$main" >/dev/null || main=main
    base=$(git merge-base HEAD "$main")
    if [[ "$base" == "$(git rev-parse HEAD)" ]]; then
        base=$(git rev-parse HEAD^)
    fi
fi
base=$(git rev-parse --verify "$base^{commit}")

# The baseline and the comparison share Gungraun's output directory. The
# baseline builds in a target directory of its own, kept between runs:
# sharing one would let cargo take the baseline's build of a workspace
# crate for this checkout's.
home="$root/target/gungraun-gate"
worktree="$root/target/bench-base"
base_target="$root/target/bench-base-target"
rm -rf "$home"
# A worktree that a `cargo clean` deleted is still registered until pruned.
cleanup() {
    git worktree remove --force "$worktree" >/dev/null 2>&1 || true
    git worktree prune
}
trap cleanup EXIT
cleanup

bench=(cargo bench --locked -p folio-testkit --bench compile --)
step "baseline: $(git log -1 --format='%h %s' "$base")"
git worktree add --detach "$worktree" "$base" >/dev/null
if [[ -f "$worktree/crates/folio-testkit/benches/compile.rs" ]]; then
    (cd "$worktree" && CARGO_TARGET_DIR="$base_target" "${bench[@]}" --save-baseline=base --home="$home")
    compare=(--baseline=base)
else
    echo "the baseline has no benchmarks; recording this checkout only"
    compare=()
fi

step "this checkout: $(git log -1 --format='%h %s' HEAD)$(git diff --quiet HEAD || echo ' (with changes)')"
# Gungraun exits with code 3 when a benchmark exceeds its regression limit
# of 3% more instructions, which the benchmarks set.
"${bench[@]}" ${compare[@]+"${compare[@]}"} --home="$home"
step "no regressions"
