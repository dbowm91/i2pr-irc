#!/bin/sh
set -eu
mode=${1:-quick}
./scripts/check-network-boundary.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
if [ "$mode" = full ]; then ./scripts/fuzz-smoke.sh; fi
