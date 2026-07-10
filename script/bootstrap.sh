#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

say() { printf "\n==> %s\n" "$*"; }
die() { printf "\nERROR: %s\n" "$*" >&2; exit 1; }
need_cmd() { command -v "$1" >/dev/null 2>&1 || die "Missing required command: $1"; }

check_system_deps() {
  say "Checking system dependencies"
  need_cmd git
  need_cmd mise

  if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1; then
    die "C compiler not found. Install build tools first (build-essential / gcc / Xcode CLT)."
  fi
}

install_mise_tools() {
  say "Installing tools from mise.toml"
  if [[ -f "mise.lock" ]]; then
    mise install --locked
  else
    say "No mise.lock found — installing without lockfile, then generating one"
    MISE_LOCKED=0 mise install && mise lock
  fi
}

sanity() {
  say "Sanity check"
  # Use `mise x` to evaluate mise.toml env (CARGO_HOME, _.path, etc.)
  mise x -- rustc --version
  mise x -- cargo --version
  mise x -- just --version
  mise x -- cargo nextest --version || true
  mise x -- cargo deny --version || true
}

main() {
  check_system_deps
  install_mise_tools
  sanity

  say "Bootstrap complete"
  say "Usage: 'just build', 'just test', 'just check'"
  say "Or directly: 'mise x -- cargo build'"
}

main "$@"
