#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
readonly repository_root
cd -- "$repository_root"

heading() {
    local name=$1
    printf '\n=== %s ===\n' "$name"
}

run_workspace_deny() {
    heading "cargo deny"
    cargo deny check
}

run_audit_tool_deny() {
    heading "cargo deny (dependency audit tool)"
    cargo deny --manifest-path tools/dependency-unsafe/Cargo.toml --locked check --config tools/dependency-unsafe/deny.toml
}

run_workspace_machete() {
    heading "cargo machete"
    cargo machete
}

run_audit_tool_machete() {
    heading "cargo machete (dependency audit tool)"
    cargo machete --skip-target-dir tools/dependency-unsafe
}

run_workspace_audit() {
    heading "cargo audit"
    cargo audit
}

run_audit_tool_audit() {
    heading "cargo audit (dependency audit tool)"
    cargo audit --file tools/dependency-unsafe/Cargo.lock
}

failed=0
if ! run_workspace_deny; then
    failed=1
fi
if ! run_audit_tool_deny; then
    failed=1
fi
if ! run_workspace_machete; then
    failed=1
fi
if ! run_audit_tool_machete; then
    failed=1
fi
if ! run_workspace_audit; then
    failed=1
fi
if ! run_audit_tool_audit; then
    failed=1
fi

if (( failed != 0 )); then
    printf '\nOne or more dependency audit checks failed.\n' >&2
    exit 1
fi

printf '\nAll dependency audit checks passed.\n'
