#!/usr/bin/env bash
set -euo pipefail

if [[ $(basename -- "$0") == claude ]]; then
    capture=${LOCALHOLD_CLAUDE_TEST_CAPTURE:?}
    mkdir -p -- "$capture"
    printf '%s\n' "$@" > "$capture/args"
    printf '%s\n' "$TMPDIR" "$TMP" "$TEMP" > "$capture/temp-environment"
    pwd -P > "$capture/cwd"
    mkdir -p -- "$TMPDIR/nested"
    printf 'temporary review data\n' > "$TMPDIR/nested/payload"
    printf 'fake review output\n'
    exit "${LOCALHOLD_CLAUDE_TEST_EXIT:-0}"
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

cleanup() {
    local status=$?
    trap - EXIT
    case "$test_root" in
        "$cache_root"/claude-review-test.*) rm -rf -- "$test_root" ;;
        *)
            printf 'refusing to remove unexpected test path: %s\n' "$test_root" >&2
            status=1
            ;;
    esac
    exit "$status"
}
trap cleanup EXIT

mkdir -- "$test_root/bin" "$test_root/capture"
ln -s -- "$script_dir/test_claude_review.sh" "$test_root/bin/claude"

PATH="$test_root/bin:$PATH" \
LOCALHOLD_CLAUDE_TEST_CAPTURE="$test_root/capture" \
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
if [[ $(< "$test_root/capture/cwd") != "$repository_root" ]]; then
    printf 'Claude reviewer did not run from the repository root\n' >&2
    exit 1
fi

rm -rf -- "$test_root/capture"
mkdir -- "$test_root/capture"
set +e
PATH="$test_root/bin:$PATH" \
LOCALHOLD_CLAUDE_TEST_CAPTURE="$test_root/capture" \
LOCALHOLD_CLAUDE_TEST_EXIT=23 \
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

if "$repository_root/script/claude-review.sh" sonnet "unsupported model" >/dev/null 2>&1; then
    printf 'Claude review wrapper accepted an unsupported model\n' >&2
    exit 1
fi

printf 'Claude review isolation checks passed\n'
