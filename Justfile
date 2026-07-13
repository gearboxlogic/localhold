# LocalHold — common development recipes
# Run `just --list` to see all available recipes.

# Build the project
build:
    cargo build

# Build the project in release mode
build-release:
    cargo build --release --locked --features reranker

# Explicit alias: release builds already ship with reranker support compiled in
build-release-reranker:
    just build-release

# Build release with opportunistic CUDA acceleration for the reranker
build-release-reranker-cuda:
    cargo build --release --locked --features reranker-cuda

# Run tests via nextest
test *ARGS:
    cargo nextest run {{ ARGS }}

# Run black-box end-to-end tests against the real hold binary
test-black-box:
    just build-release
    LOCALHOLD_BLACK_BOX_BIN="$PWD/target/release/hold" cargo test --test black_box -- --ignored --nocapture

# Run all ignored integration soak/stress tests
test-integration-ignored:
    cargo test --test integration --locked -- --ignored --nocapture

# Run Docker-backed PostgreSQL pgvector bootstrap smoke test
test-postgres-smoke:
    bash ./script/test-postgres-smoke.sh

# Run ignored soak/regression tests
test-soak:
    cargo test tracked_embedding_tasks_are_reaped_under_sustained_load -- --ignored

# Run clippy with deny warnings
clippy:
    cargo clippy --all-targets --all-features --locked -- -D warnings

# Format all code (requires nightly: `rustup toolchain install nightly -c rustfmt`)
fmt:
    rustup run nightly cargo-fmt --all

# Check formatting (requires nightly)
fmt-check:
    rustup run nightly cargo-fmt --all -- --check

# Run cargo-deny supply chain audit
deny:
    cargo deny check

# Check direct workspace dependencies for newer available versions
outdated:
    cargo outdated --workspace --root-deps-only --ignore localhold --exit-code 1

# Run full dependency audit (deny + machete + cargo-audit)
audit:
    ./script/dep-audit.sh

# Reject internal artifacts, private paths, and legacy product identity
hygiene:
    ./script/check-publication-hygiene.sh

# Prevent production timing logic from bypassing the injectable clock
time-abstraction:
    ./script/check-time-abstraction.sh

# CI-style gate: fmt + clippy + deny + tests
check:
    just hygiene
    just time-abstraction
    just fmt-check
    just clippy
    just deny
    just test

# Bootstrap local development environment (toolchains + deps)
setup:
    just setup-tools
    just backend-install

# Install all tools via mise
setup-tools:
    mise install

# Prefetch Rust dependencies
backend-install:
    mise x -- cargo fetch

# Regenerate mise.lock after version changes
lock:
    MISE_LOCKED=0 mise install && mise lock

# Check for unused dependencies
unused-deps:
    cargo machete

# Run all benchmarks
bench:
    cargo bench

# Run search latency benchmark
bench-search:
    cargo bench --bench search_latency

# Run store batch benchmark
bench-store:
    cargo bench --bench store_batch

# Run embed throughput benchmark
bench-embed:
    cargo bench --bench embed_throughput

# Run memory footprint report
bench-footprint:
    cargo bench --bench memory_footprint

# Run quick benchmarks (limited iterations)
bench-quick:
    cargo bench -- --quick
