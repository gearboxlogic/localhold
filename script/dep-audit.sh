#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    printf 'dependency audit must run inside a Git repository\n' >&2
    exit 1
}
cd "$repo_root"

failed=0

run_check() {
    local name="$1"
    shift

    printf '\n=== %s ===\n' "$name"
    if ! "$@"; then
        failed=1
    fi
}

run_check "cargo deny" cargo deny check
run_check "cargo machete" cargo machete
run_check "cargo audit" cargo audit

if (( failed != 0 )); then
    printf '\nOne or more dependency audit checks failed.\n' >&2
    exit 1
fi

printf '\nAll dependency audit checks passed.\n'
