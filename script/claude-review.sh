#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat >&2 <<'EOF'
Usage: ./script/claude-review.sh <opus|fable> [PROMPT]

Runs one read-only, non-interactive Claude review with MCP servers and other
customizations disabled. If PROMPT is omitted, Claude reads it from stdin.
EOF
}

if (( $# < 1 || $# > 2 )); then
    usage
    exit 2
fi

model=$1
case "$model" in
    opus|fable) ;;
    *)
        printf 'unsupported Claude review model: %s\n' "$model" >&2
        usage
        exit 2
        ;;
esac
shift

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repository_root=$(cd -- "$script_dir/.." && pwd -P)
cache_root="$repository_root/.cache"
scratch_root="$cache_root/claude-reviews"

ensure_private_directory() {
    local path=$1
    if [[ -L "$path" || ( -e "$path" && ! -d "$path" ) ]]; then
        printf 'Claude review scratch path is not a regular directory: %s\n' "$path" >&2
        exit 1
    fi
    if [[ ! -d "$path" ]]; then
        mkdir -- "$path"
    fi
}

umask 077
ensure_private_directory "$cache_root"
ensure_private_directory "$scratch_root"
scratch_directory=$(mktemp -d "$scratch_root/session.XXXXXXXXXX")

cleanup() {
    local status=$?
    trap - EXIT HUP INT TERM
    case "$scratch_directory" in
        "$scratch_root"/session.*)
            if ! rm -rf -- "$scratch_directory"; then
                printf 'failed to remove Claude review scratch directory: %s\n' "$scratch_directory" >&2
                if (( status == 0 )); then
                    status=1
                fi
            fi
            ;;
        *)
            printf 'refusing to remove unexpected Claude review scratch path: %s\n' "$scratch_directory" >&2
            if (( status == 0 )); then
                status=1
            fi
            ;;
    esac
    rmdir -- "$scratch_root" 2>/dev/null || true
    exit "$status"
}

trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

claude_binary=$(command -v claude || true)
if [[ -z "$claude_binary" ]]; then
    printf 'Claude CLI is not installed or is not on PATH\n' >&2
    exit 1
fi

prompt=()
if (( $# == 1 )); then
    prompt=(-- "$1")
fi

cd -- "$repository_root"
TMPDIR="$scratch_directory" \
TMP="$scratch_directory" \
TEMP="$scratch_directory" \
"$claude_binary" \
    --safe-mode \
    --mcp-config '{"mcpServers":{}}' \
    --strict-mcp-config \
    --disable-slash-commands \
    --no-chrome \
    --no-session-persistence \
    --model "$model" \
    --effort high \
    --permission-mode plan \
    --tools Read,Grep,Glob,Bash \
    --print \
    --output-format text \
    "${prompt[@]}"
