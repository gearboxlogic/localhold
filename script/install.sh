#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
readonly repository_root
prefix="${LOCALHOLD_PREFIX:-$HOME/.local}"
destdir="${DESTDIR:-}"
profile="cpu"
build_dir="${LOCALHOLD_BUILD_DIR:-${CARGO_TARGET_DIR:-$repository_root/target}}"
cargo_command="${CARGO:-cargo}"
readonly build_dir cargo_command

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
  cpu|cuda) ;;
  *) printf 'error: unsupported profile: %s\n' "$profile" >&2; exit 2 ;;
esac

need_command() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'error: required build command not found: %s\n' "$1" >&2
    exit 1
  }
}

need_one_of() {
  local description="$1"
  shift
  local command
  for command in "$@"; do
    command -v "$command" >/dev/null 2>&1 && return 0
  done
  printf 'error: %s is required (tried: %s)\n' "$description" "$*" >&2
  exit 1
}

need_command "$cargo_command"
need_command cmake
need_one_of "a C compiler" cc gcc clang
need_one_of "a C++ compiler" c++ g++ clang++
need_one_of "Make or Ninja" make ninja

if [[ "$(uname -s)" == "Linux" ]]; then
  need_command pkg-config
  pkg-config --exists openssl || {
    printf '%s\n' 'error: OpenSSL development files are required (for example, openssl-devel or libssl-dev)' >&2
    exit 1
  }
fi

cd -- "$repository_root"
case "$profile" in
  cpu) "$cargo_command" build --release --locked --features reranker --target-dir "$build_dir" ;;
  cuda) "$cargo_command" build --release --locked --features reranker-cuda --target-dir "$build_dir" ;;
esac

bin_dir="${destdir}${prefix}/bin"
share_dir="${destdir}${prefix}/share/localhold"
doc_dir="${destdir}${prefix}/share/doc/localhold"
mkdir -p "$bin_dir" "$share_dir" "$doc_dir"
install -m 0755 "$build_dir/release/hold" "$bin_dir/hold"
install -m 0644 localhold.example.toml "$share_dir/localhold.example.toml"
install -m 0644 LICENSE NOTICE THIRD_PARTY_NOTICES.md "$doc_dir/"

printf 'Installed LocalHold (%s) to %s\n' "$profile" "$bin_dir/hold"
