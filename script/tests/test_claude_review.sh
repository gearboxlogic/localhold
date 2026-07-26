#!/usr/bin/env bash
set -euo pipefail

if [[ $(basename -- "$0") == claude ]]; then
    fake_root=$(cd -- "$(dirname -- "$0")/.." && pwd -P)
    capture="$fake_root/capture"
    mkdir -p -- "$capture"
    printf '%s\n' "$@" > "$capture/args"
    printf '%s\n' "$TMPDIR" "$TMP" "$TEMP" > "$capture/temp-environment"
    env | LC_ALL=C sort > "$capture/environment"
    pwd -P > "$capture/cwd"
    mkdir -p -- "$TMPDIR/nested"
    printf 'temporary review data\n' > "$TMPDIR/nested/payload"
    if [[ " $* " == *" Wait for a termination signal. "* ]]; then
        trap 'printf "TERM\n" > "$capture/signal"; exit 0' TERM
        printf '%s\n' "$BASHPID" > "$capture/child-pid"
        : > "$capture/ready"
        while :; do
            sleep 0.1
        done
    fi
    printf 'fake review output\n'
    if [[ " $* " == *" Fail this fake review. "* ]]; then
        exit 23
    fi
    exit 0
fi

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repository_root=$(cd -- "$script_dir/../.." && pwd -P)
cache_root="$repository_root/.cache"

if [[ -L "$cache_root" || ( -e "$cache_root" && ! -d "$cache_root" ) ]]; then
    printf 'test cache path is not a regular directory: %s\n' "$cache_root" >&2
    exit 1
fi
mkdir -p -- "$cache_root"
test_root=$(mktemp -d "$cache_root/claude-review-test.XXXXXXXXXX")
scratch_root="$cache_root/claude-reviews"
scratch_root_created=false
wrapper_pid=
watchdog_pid=
if [[ ! -d "$scratch_root" ]]; then
    mkdir -- "$scratch_root"
    scratch_root_created=true
fi

cleanup() {
    local status=$?
    trap - EXIT
    if [[ -n "$watchdog_pid" ]] && kill -0 "$watchdog_pid" 2>/dev/null; then
        kill -TERM "$watchdog_pid" 2>/dev/null || true
        wait "$watchdog_pid" 2>/dev/null || true
    fi
    if [[ -n "$wrapper_pid" ]] && kill -0 "$wrapper_pid" 2>/dev/null; then
        kill -TERM "$wrapper_pid" 2>/dev/null || true
        wait "$wrapper_pid" 2>/dev/null || true
    fi
    case "$test_root" in
        "$cache_root"/claude-review-test.*) rm -rf -- "$test_root" ;;
        *)
            printf 'refusing to remove unexpected test path: %s\n' "$test_root" >&2
            status=1
            ;;
    esac
    if [[ "$scratch_root_created" == true ]]; then
        rmdir -- "$scratch_root" 2>/dev/null || true
    fi
    exit "$status"
}
trap cleanup EXIT

mkdir -- "$test_root/bin" "$test_root/capture"
ln -s -- "$script_dir/test_claude_review.sh" "$test_root/bin/claude"

PATH="$test_root/bin:$PATH" \
LOCALHOLD_SECRET="must not reach Claude" \
ANTHROPIC_API_KEY="must not reach Claude" \
GH_TOKEN="must not reach Claude" \
AWS_SECRET_ACCESS_KEY="must not reach Claude" \
"$repository_root/script/claude-review.sh" opus "Review the LocalHold diff." > "$test_root/output"

args="$test_root/capture/args"
for expected in \
    --safe-mode \
    --strict-mcp-config \
    --mcp-config \
    '{"mcpServers":{}}' \
    --disable-slash-commands \
    --no-chrome \
    --no-session-persistence \
    --model \
    opus \
    --effort \
    high \
    --permission-mode \
    plan \
    --tools \
    Read,Grep,Glob,Bash \
    --print \
    --output-format \
    text \
    "Review the LocalHold diff."
do
    grep -Fqx -- "$expected" "$args"
done
if grep -Fqx -- max "$args"; then
    printf 'Claude review wrapper selected max effort\n' >&2
    exit 1
fi

mapfile -t temp_environment < "$test_root/capture/temp-environment"
if (( ${#temp_environment[@]} != 3 )); then
    printf 'Claude review wrapper did not set all temporary environment variables\n' >&2
    exit 1
fi
if [[ ${temp_environment[0]} != "${temp_environment[1]}" ]]; then
    printf 'Claude review TMPDIR and TMP do not share one directory\n' >&2
    exit 1
fi
if [[ ${temp_environment[0]} != "${temp_environment[2]}" ]]; then
    printf 'Claude review temporary environment variables do not share one directory\n' >&2
    exit 1
fi
case "${temp_environment[0]}" in
    "$repository_root"/.cache/claude-reviews/session.*) ;;
    *)
        printf 'Claude review scratch escaped the repository cache: %s\n' "${temp_environment[0]}" >&2
        exit 1
        ;;
esac
if [[ -e ${temp_environment[0]} ]]; then
    printf 'Claude review scratch survived successful completion: %s\n' "${temp_environment[0]}" >&2
    exit 1
fi
if [[ ! -d "$scratch_root" ]]; then
    printf 'Claude review wrapper removed the pre-existing scratch root\n' >&2
    exit 1
fi
if [[ $(< "$test_root/capture/cwd") != "$repository_root" ]]; then
    printf 'Claude reviewer did not run from the repository root\n' >&2
    exit 1
fi
if grep -Eq '^(LOCALHOLD_|ANTHROPIC_API_KEY=|GH_TOKEN=|AWS_SECRET_ACCESS_KEY=)' "$test_root/capture/environment"; then
    printf 'Claude reviewer inherited a sensitive environment variable\n' >&2
    exit 1
fi

rm -rf -- "$test_root/capture"
mkdir -- "$test_root/capture"
set +e
PATH="$test_root/bin:$PATH" \
"$repository_root/script/claude-review.sh" fable "Fail this fake review." > "$test_root/failure-output"
status=$?
set -e
if (( status != 23 )); then
    printf 'Claude review wrapper did not preserve reviewer exit status: %d\n' "$status" >&2
    exit 1
fi
failure_scratch=$(sed -n '1p' "$test_root/capture/temp-environment")
if [[ -e "$failure_scratch" ]]; then
    printf 'Claude review scratch survived failed completion: %s\n' "$failure_scratch" >&2
    exit 1
fi

rm -rf -- "$test_root/capture"
mkdir -- "$test_root/capture"
PATH="$test_root/bin:$PATH" \
"$repository_root/script/claude-review.sh" opus "Wait for a termination signal." > "$test_root/signal-output" &
wrapper_pid=$!
for _ in {1..200}; do
    if [[ -e "$test_root/capture/ready" ]]; then
        break
    fi
    sleep 0.01
done
if [[ ! -e "$test_root/capture/ready" ]]; then
    printf 'fake Claude reviewer did not become ready for the signal test\n' >&2
    exit 1
fi
child_pid=$(< "$test_root/capture/child-pid")
kill -TERM "$wrapper_pid"
timeout_marker="$test_root/wrapper-timeout"
(
    sleep 10
    if kill -0 "$wrapper_pid" 2>/dev/null; then
        : > "$timeout_marker"
        kill -KILL "$wrapper_pid" 2>/dev/null || true
    fi
) &
watchdog_pid=$!
set +e
wait "$wrapper_pid"
status=$?
kill -TERM "$watchdog_pid" 2>/dev/null
wait "$watchdog_pid" 2>/dev/null
set -e
wrapper_pid=
watchdog_pid=
if [[ -e "$timeout_marker" ]]; then
    signal_scratch=$(sed -n '1p' "$test_root/capture/temp-environment")
    case "$signal_scratch" in
        "$scratch_root"/session.*) rm -rf -- "$signal_scratch" ;;
        *) printf 'refusing to remove unexpected timed-out scratch path: %s\n' "$signal_scratch" >&2 ;;
    esac
    printf 'Claude review wrapper did not exit within 10 seconds after SIGTERM\n' >&2
    exit 1
fi
if (( status != 143 )); then
    printf 'Claude review wrapper did not preserve the termination status: %d\n' "$status" >&2
    exit 1
fi
if [[ $(< "$test_root/capture/signal") != TERM ]]; then
    printf 'Claude review wrapper did not forward SIGTERM to the reviewer\n' >&2
    exit 1
fi
if kill -0 "$child_pid" 2>/dev/null; then
    printf 'Claude reviewer survived wrapper termination: %s\n' "$child_pid" >&2
    exit 1
fi
signal_scratch=$(sed -n '1p' "$test_root/capture/temp-environment")
if [[ -e "$signal_scratch" ]]; then
    printf 'Claude review scratch survived wrapper termination: %s\n' "$signal_scratch" >&2
    exit 1
fi

if "$repository_root/script/claude-review.sh" sonnet "unsupported model" >/dev/null 2>&1; then
    printf 'Claude review wrapper accepted an unsupported model\n' >&2
    exit 1
fi

printf 'Claude review isolation checks passed\n'
