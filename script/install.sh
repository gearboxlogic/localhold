#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
prefix="${LOCALHOLD_PREFIX:-$HOME/.local}"
destdir="${DESTDIR:-}"
profile="cpu"
build_dir="${LOCALHOLD_BUILD_DIR:-${CARGO_TARGET_DIR:-$repo_root/target}}"

usage() {
  cat <<'EOF'
Usage: ./script/install.sh [--prefix PATH] [--profile cpu|cuda]

Builds LocalHold from the locked source tree and installs:
  PREFIX/bin/hold
  PREFIX/share/localhold/localhold.example.toml
  PREFIX/share/doc/localhold/{LICENSE,NOTICE,THIRD_PARTY_NOTICES.md}

Environment:
  DESTDIR           Optional packaging root prepended to installed paths.
  LOCALHOLD_PREFIX  Default prefix when --prefix is omitted (~/.local).
  CARGO             Cargo executable to use (cargo).
  LOCALHOLD_BUILD_DIR  Build output directory (defaults to CARGO_TARGET_DIR or ./target).
EOF
}

while (($# > 0)); do
  case "$1" in
    --prefix)
      (($# >= 2)) || { printf '%s\n' 'error: --prefix requires a path' >&2; exit 2; }
      prefix="$2"
      shift 2
      ;;
    --profile)
      (($# >= 2)) || { printf '%s\n' 'error: --profile requires cpu or cuda' >&2; exit 2; }
      profile="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'error: unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$profile" in
  cpu) features="reranker" ;;
  cuda) features="reranker-cuda" ;;
  *) printf 'error: unsupported profile: %s\n' "$profile" >&2; exit 2 ;;
esac

command -v "${CARGO:-cargo}" >/dev/null 2>&1 || {
  printf 'error: Cargo is required to build LocalHold\n' >&2
  exit 1
}

cd "$repo_root"
"${CARGO:-cargo}" build --release --locked --features "$features" --target-dir "$build_dir"

bin_dir="${destdir}${prefix}/bin"
share_dir="${destdir}${prefix}/share/localhold"
doc_dir="${destdir}${prefix}/share/doc/localhold"
mkdir -p "$bin_dir" "$share_dir" "$doc_dir"
install -m 0755 "$build_dir/release/hold" "$bin_dir/hold"
install -m 0644 localhold.example.toml "$share_dir/localhold.example.toml"
install -m 0644 LICENSE NOTICE THIRD_PARTY_NOTICES.md "$doc_dir/"

printf 'Installed LocalHold (%s) to %s\n' "$profile" "$bin_dir/hold"
case ":${PATH}:" in
  *":${prefix}/bin:"*) ;;
  *) printf 'Add %s/bin to PATH before invoking hold by name.\n' "$prefix" ;;
esac
