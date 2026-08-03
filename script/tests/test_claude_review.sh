#!/usr/bin/env bash
set -euo pipefail

if [[ $(basename -- "$0") == ps ]]; then
    fake_root=$(cd -- "$(dirname -- "$0")/.." && pwd -P)
    capture="$fake_root/capture"
    simulated_state=
    if [[ -e "$capture/simulate-zombie-group" ]]; then
        simulated_state=Z
    elif [[ -e "$capture/simulate-live-group" ]]; then
        simulated_state=T
    fi
    if [[ -n $simulated_state ]]; then
        count=0
        if [[ -e "$capture/ps-count" ]]; then
            count=$(< "$capture/ps-count")
        fi
        (( count += 1 ))
        printf '%s\n' "$count" > "$capture/ps-count"
        if (( count > 101 )); then
            printf '%s %s\n' "$(< "$capture/child-pid")" "$simulated_state"
            exit 0
        fi
    fi
    command -p ps -A -o pgid=,stat=
    exit
fi

if [[ $(basename -- "$0") == claude ]]; then
    fake_root=$(cd -- "$(dirname -- "$0")/.." && pwd -P)
    capture="$fake_root/capture"
    mkdir -p -- "$capture"
    printf '%s\n' "$@" > "$capture/args"
    prompt_argument=false
    for argument in "$@"; do
        if [[ $argument == -- ]]; then
            prompt_argument=true
        fi
    done
    if [[ $prompt_argument == false ]]; then
        cat > "$capture/stdin"
    fi
    printf '%s\n' "$TMPDIR" "$TMP" "$TEMP" > "$capture/temp-environment"
    env | LC_ALL=C sort > "$capture/environment"
    pwd -P > "$capture/cwd"
    mkdir -p -- "$TMPDIR/nested"
    printf 'temporary review data\n' > "$TMPDIR/nested/payload"
    printf '%s\n' "$BASHPID" > "$capture/child-pid"

    start_resistant_grandchild() {
        rm -f -- "$capture/grandchild-ready"
        bash -c 'trap "" TERM; : > "$1"; while :; do sleep 1; done' reviewer-grandchild "$capture/grandchild-ready" &
        printf '%s\n' "$!" > "$capture/grandchild-pid"
        while [[ ! -e "$capture/grandchild-ready" ]]; do
            sleep 0.01
        done
    }

    if [[ " $* " == *" Wait for a termination signal. "* ]]; then
        trap 'printf "TERM\n" > "$capture/signal"' TERM
        start_resistant_grandchild
        printf '%s\n' "$BASHPID" > "$capture/child-pid"
        : > "$capture/ready"
        while :; do
            sleep 0.1
        done
    fi
    if [[ " $* " == *" Leave a descendant after success. "* ]]; then
        start_resistant_grandchild
        : > "$capture/ready"
    fi
    if [[ " $* " == *" Leave a descendant after failure. "* ]]; then
        start_resistant_grandchild
        : > "$capture/ready"
        exit 23
    fi
    if [[ " $* " == *" Simulate a zombie-only descendant group. "* ]]; then
        : > "$capture/simulate-zombie-group"
        start_resistant_grandchild
        : > "$capture/ready"
    fi
    if [[ " $* " == *" Simulate a stopped descendant group. "* ]]; then
        : > "$capture/simulate-live-group"
        start_resistant_grandchild
        : > "$capture/ready"
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
child_pid=
grandchild_pid=
if [[ ! -d "$scratch_root" ]]; then
    mkdir -- "$scratch_root"
    scratch_root_created=true
fi

terminate_fake_reviewer() {
    if [[ -z "$child_pid" ]] || ! kill -0 "$child_pid" 2>/dev/null; then
        return
    fi
    kill -TERM "$child_pid" 2>/dev/null || true
    for _ in {1..100}; do
        if ! kill -0 "$child_pid" 2>/dev/null; then
            return
        fi
        sleep 0.01
    done
    kill -KILL "$child_pid" 2>/dev/null || true
}

terminate_fake_grandchild() {
    if [[ -z "$grandchild_pid" ]] || ! kill -0 "$grandchild_pid" 2>/dev/null; then
        return
    fi
    kill -TERM "$grandchild_pid" 2>/dev/null || true
    for _ in {1..100}; do
        if ! kill -0 "$grandchild_pid" 2>/dev/null; then
            return
        fi
        sleep 0.01
    done
    kill -KILL "$grandchild_pid" 2>/dev/null || true
}

cleanup() {
    local status=$?
    trap - EXIT
    if [[ -n "$watchdog_pid" ]] && kill -0 "$watchdog_pid" 2>/dev/null; then
        kill -TERM "$watchdog_pid" 2>/dev/null || true
        wait "$watchdog_pid" 2>/dev/null || true
    fi
    terminate_fake_reviewer
    terminate_fake_grandchild
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
ln -s -- "$script_dir/test_claude_review.sh" "$test_root/bin/ps"

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

mapfile -t -- temp_environment < "$test_root/capture/temp-environment"
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
if grep -Eq '^(LOCALHOLD_|ANTHROPIC_API_KEY=|GH_TOKEN=|AWS_SECRET_ACCESS_KEY=)|must not reach Claude' "$test_root/capture/environment"; then
    printf 'Claude reviewer inherited a sensitive environment variable\n' >&2
    exit 1
fi

rm -rf -- "$test_root/capture"
mkdir -- "$test_root/capture"
printf 'Review from standard input.\n' |
    PATH="$test_root/bin:$PATH" \
        "$repository_root/script/claude-review.sh" opus > "$test_root/stdin-output"
if [[ $(< "$test_root/capture/stdin") != "Review from standard input." ]]; then
    printf 'Claude review wrapper did not forward the standard-input prompt\n' >&2
    exit 1
fi

rm -rf -- "$test_root/capture"
mkdir -- "$test_root/capture"
if PATH="$test_root/bin:$PATH" \
    "$repository_root/script/claude-review.sh" fable "Fail this fake review." > "$test_root/failure-output"
then
    status=0
else
    status=$?
fi
if (( status != 23 )); then
    printf 'Claude review wrapper did not preserve reviewer exit status: %d\n' "$status" >&2
    exit 1
fi
failure_scratch=$(sed -n '1p' "$test_root/capture/temp-environment")
if [[ -e "$failure_scratch" ]]; then
    printf 'Claude review scratch survived failed completion: %s\n' "$failure_scratch" >&2
    exit 1
fi

process_is_live() {
    local pid=$1 state
    kill -0 "$pid" 2>/dev/null || return 1
    state=$(command -p ps -o stat= -p "$pid" 2>/dev/null) || return 1
    [[ -n $state && $state != Z* ]]
}

assert_descendant_is_drained() {
    local prompt=$1
    local expected_status=$2
    rm -rf -- "$test_root/capture"
    mkdir -- "$test_root/capture"
    if PATH="$test_root/bin:$PATH" "$repository_root/script/claude-review.sh" opus "$prompt" > "$test_root/descendant-output" 2> "$test_root/descendant-error"; then
        status=0
    else
        status=$?
    fi
    if (( status != expected_status )); then
        printf 'Claude review wrapper changed descendant test status: expected=%d actual=%d\n' "$expected_status" "$status" >&2
        exit 1
    fi
    if (( expected_status == 1 )); then
        grep -Fq 'Claude review process group survived TERM and KILL' "$test_root/descendant-error"
    fi
    grandchild_pid=$(< "$test_root/capture/grandchild-pid")
    if process_is_live "$grandchild_pid"; then
        printf 'Claude reviewer descendant survived completion: %s\n' "$grandchild_pid" >&2
        exit 1
    fi
    grandchild_pid=
    descendant_scratch=$(sed -n '1p' "$test_root/capture/temp-environment")
    if [[ -e "$descendant_scratch" ]]; then
        printf 'Claude review scratch survived descendant cleanup: %s\n' "$descendant_scratch" >&2
        exit 1
    fi
}

assert_descendant_is_drained "Leave a descendant after success." 0
assert_descendant_is_drained "Leave a descendant after failure." 23
assert_descendant_is_drained "Simulate a zombie-only descendant group." 0
assert_descendant_is_drained "Simulate a stopped descendant group." 1

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
grandchild_pid=$(< "$test_root/capture/grandchild-pid")
kill -TERM "$wrapper_pid"
for _ in {1..200}; do
    if [[ -e "$test_root/capture/signal" ]]; then
        break
    fi
    sleep 0.01
done
if [[ ! -e "$test_root/capture/signal" ]]; then
    printf 'fake Claude reviewer did not receive the first termination signal\n' >&2
    exit 1
fi
# Repeated signals during the resistant-child grace period must not interrupt
# the wrapper's bounded TERM-to-KILL drain or scratch cleanup.
kill -TERM "$wrapper_pid" 2>/dev/null || true
timeout_marker="$test_root/wrapper-timeout"
(
    sleep 10
    if kill -0 "$wrapper_pid" 2>/dev/null; then
        : > "$timeout_marker"
        terminate_fake_reviewer
        kill -KILL "$wrapper_pid" 2>/dev/null || true
    fi
) &
watchdog_pid=$!
if wait "$wrapper_pid"; then
    status=0
else
    status=$?
fi
kill -TERM "$watchdog_pid" 2>/dev/null || true
wait "$watchdog_pid" 2>/dev/null || true
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
child_pid=
for _ in {1..100}; do
    if ! kill -0 "$grandchild_pid" 2>/dev/null; then
        break
    fi
    sleep 0.01
done
if kill -0 "$grandchild_pid" 2>/dev/null; then
    printf 'Claude reviewer grandchild survived wrapper termination: %s\n' "$grandchild_pid" >&2
    exit 1
fi
grandchild_pid=
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
