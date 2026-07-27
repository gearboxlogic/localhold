#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd -- "$repository_root"

cargo fetch --manifest-path tools/maintainability/Cargo.toml --locked
cargo fmt --manifest-path tools/maintainability/Cargo.toml -- --check
cargo test --manifest-path tools/maintainability/Cargo.toml --locked
cargo clippy --manifest-path tools/maintainability/Cargo.toml --all-targets --locked -- -D warnings
cargo run --manifest-path tools/maintainability/Cargo.toml --locked -- check
