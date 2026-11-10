#!/usr/bin/env bash
# Run before every commit: formatting, lints and tests.
# Stops at the first failure (set -e) and treats a failure anywhere in a
# pipeline as a failure (pipefail).
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== cargo fmt --check"
cargo fmt --check
echo "== cargo clippy"
cargo clippy --all-targets --quiet -- -D warnings
echo "== cargo test"
cargo test --quiet
echo "== all checks passed"
