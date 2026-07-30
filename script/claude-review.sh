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

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
readonly repository_root
cache_root="$repository_root/.cache"
scratch_root="$cache_root/claude-reviews"
claude_pid=

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
    if [[ -n "$claude_pid" ]] && kill -0 "$claude_pid" 2>/dev/null; then
        kill -TERM "$claude_pid" 2>/dev/null || true
        wait "$claude_pid" 2>/dev/null || true
    fi
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
    exit "$status"
}

forward_signal() {
    local signal=$1
    local status=$2
    trap - "$signal"
    if [[ -n "$claude_pid" ]] && kill -0 "$claude_pid" 2>/dev/null; then
        kill -s "$signal" "$claude_pid" 2>/dev/null || true
        wait "$claude_pid" 2>/dev/null || true
    fi
    claude_pid=
    exit "$status"
}

trap cleanup EXIT
trap 'forward_signal HUP 129' HUP
trap 'forward_signal INT 130' INT
trap 'forward_signal TERM 143' TERM

claude_binary=$(command -v claude || true)
if [[ -z "$claude_binary" ]]; then
    printf 'Claude CLI is not installed or is not on PATH\n' >&2
    exit 1
fi
if [[ -z ${HOME:-} || -z ${PATH:-} ]]; then
    printf 'Claude review requires HOME and PATH\n' >&2
    exit 1
fi

review_environment=(
    env -i
    "HOME=$HOME"
    "PATH=$PATH"
)
for name in USER LOGNAME SHELL TERM LANG LC_ALL LC_CTYPE NO_COLOR; do
    if [[ -v $name ]]; then
        review_environment+=("$name=${!name}")
    fi
done

prompt=()
if (( $# == 1 )); then
    prompt=(-- "$1")
fi

cd -- "$repository_root"
set +e
"${review_environment[@]}" \
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
    "${prompt[@]}" &
# The child is a monitored shell job so this wrapper can forward termination
# signals. The wrapper remains synchronous to its caller and immediately waits.
claude_pid=$!
wait "$claude_pid"
status=$?
claude_pid=
set -e
exit "$status"
