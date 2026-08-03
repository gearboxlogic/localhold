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
claude_pgid=

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

review_group_is_alive() {
    [[ -n "$claude_pgid" ]] && kill -0 -- "-$claude_pgid" 2>/dev/null
}

wait_for_review_group_exit() {
    local attempt
    for (( attempt = 0; attempt < 100; attempt++ )); do
        if ! review_group_is_alive; then
            return 0
        fi
        sleep 0.02
    done
    return 1
}

drain_review_group() {
    if ! review_group_is_alive; then
        claude_pgid=
        return 0
    fi
    kill -TERM -- "-$claude_pgid" 2>/dev/null || true
    if wait_for_review_group_exit; then
        claude_pgid=
        return 0
    fi
    kill -KILL -- "-$claude_pgid" 2>/dev/null || true
    if wait_for_review_group_exit; then
        claude_pgid=
        return 0
    fi
    printf 'Claude review process group survived TERM and KILL: %s\n' "$claude_pgid" >&2
    return 1
}

cleanup() {
    local status=$1
    # Once teardown starts, repeated termination requests must not bypass the
    # bounded process-group drain or scratch removal.
    trap '' HUP INT TERM
    trap - EXIT
    if ! drain_review_group && (( status == 0 )); then
        status=1
    fi
    if [[ -n "$claude_pid" ]]; then
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
    local status=$1
    trap '' HUP INT TERM
    # EXIT cleanup performs bounded process-group termination. Keeping one
    # shutdown path prevents a signal-resistant reviewer from blocking here.
    exit "$status"
}

trap 'cleanup "$?"' EXIT
trap 'forward_signal 129' HUP
trap 'forward_signal 130' INT
trap 'forward_signal 143' TERM

claude_binary=$(command -v claude || true)
if [[ -z "$claude_binary" ]]; then
    printf 'Claude CLI is not installed or is not on PATH\n' >&2
    exit 1
fi
if [[ -z ${HOME:-} || -z ${PATH:-} ]]; then
    printf 'Claude review requires HOME and PATH\n' >&2
    exit 1
fi

run_claude() {
    exec env -i \
        "HOME=$HOME" \
        "PATH=$PATH" \
        "USER=${USER:-}" \
        "LOGNAME=${LOGNAME:-}" \
        "SHELL=${SHELL:-}" \
        "TERM=${TERM:-}" \
        "LANG=${LANG:-}" \
        "LC_ALL=${LC_ALL:-}" \
        "LC_CTYPE=${LC_CTYPE:-}" \
        "NO_COLOR=${NO_COLOR:-}" \
        "TMPDIR=$scratch_directory" \
        "TMP=$scratch_directory" \
        "TEMP=$scratch_directory" \
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
        "$@"
}

cd -- "$repository_root"
set +e
# Non-interactive Bash otherwise places background children in the wrapper's
# process group. Monitor mode gives each review its own group so every tool or
# shell process spawned by Claude is terminated with the review.
set -m
if (( $# == 1 )); then
    run_claude -- "$1" &
else
    run_claude <&0 &
fi
# The child is a monitored shell job so this wrapper can forward termination
# signals. The wrapper remains synchronous to its caller and immediately waits.
claude_pid=$!
claude_pgid=$claude_pid
set +m
wait "$claude_pid"
status=$?
claude_pid=
set -e
exit "$status"
