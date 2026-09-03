#!/usr/bin/env bash
# Run exactly what CI runs, in the same order, with the same flags.
#
# This exists because a near-miss is worse than no check: PR #3 was pushed after
# `RUSTFLAGS="-D warnings" cargo clippy` reported clean, but CI runs
# `cargo clippy -- -D warnings`. Those are not the same -- RUSTFLAGS reaches
# rustc, not clippy's own lints -- so the local check passed while CI failed on
# all three platforms.
set -euo pipefail

echo "==> cargo fmt --all -- --check"
cargo fmt --all -- --check

echo "==> cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> cargo test --workspace --all-targets"
cargo test --workspace --all-targets

echo "==> cargo test --workspace --doc"
cargo test --workspace --doc

echo "==> harness integrity"
python3 -m compileall -q harness scripts
python3 harness/lib/validate_registry.py
python3 harness/lib/check_no_harness_leak.py
python3 harness/lib/check_scorecard_integrity.py

echo
echo "all CI checks passed locally"
echo "note: CI pins the toolchain via rust-toolchain.toml, so this is the same"
echo "      compiler and the same lint set -- not an approximation of them."
