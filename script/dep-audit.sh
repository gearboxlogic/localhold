#!/usr/bin/env bash
set -euo pipefail

# Dependency audit script — runs all supply-chain/hygiene checks.
# Exit code is non-zero if any check fails.

failed=0

echo "=== cargo deny ==="
if ! cargo deny check; then
    failed=1
fi

echo ""
echo "=== cargo machete ==="
if ! cargo machete; then
    failed=1
fi

echo ""
echo "=== cargo audit ==="
if ! cargo audit; then
    failed=1
fi

if [ "$failed" -ne 0 ]; then
    echo ""
    echo "One or more audit checks failed."
    exit 1
fi

echo ""
echo "All audit checks passed."
