#!/usr/bin/env bash
# Runs every check a change must pass before it is pushed: build, tests
# (including the dependency-direction check), clippy, rustfmt, rustdoc, the
# MSRV check and cargo-deny. Usage: scripts/check.sh [--fast]
#   --fast  skip the MSRV check and cargo-deny
set -euo pipefail
cd "$(dirname "$0")/.."

fast=false
if [[ "${1:-}" == "--fast" ]]; then fast=true; fi

# Keep in step with `rust-version` in Cargo.toml.
msrv=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml)
if [[ -z "$msrv" ]]; then echo "could not read rust-version from Cargo.toml" >&2; exit 1; fi

step() { printf '\n==> %s\n' "$*"; }

step "rustfmt"
cargo fmt --all --check

step "build"
cargo build --workspace --all-targets --locked

step "test"
cargo test --workspace --all-features --locked

step "clippy"
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

step "rustdoc"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked

if $fast; then
    step "skipped MSRV and cargo-deny (--fast)"
    exit 0
fi

step "MSRV $msrv"
if ! rustup run "$msrv" rustc --version >/dev/null 2>&1; then
    echo "Rust $msrv is not installed: rustup toolchain install $msrv --profile minimal" >&2
    exit 1
fi
cargo "+$msrv" check --workspace --all-targets --all-features --locked

step "cargo-deny"
if ! cargo deny --version >/dev/null 2>&1; then
    echo "cargo-deny is not installed: cargo install cargo-deny --locked" >&2
    exit 1
fi
cargo deny check

step "all checks passed"
