#!/usr/bin/env bash
set -euo pipefail

pattern='(chrono::)?Utc::now\(|SystemTime::now\(|(tokio::time::|std::time::)?Instant::now\(|tokio::time::(sleep|sleep_until|interval|timeout)\(|(std::)?thread::sleep\('
failed=0

while IFS= read -r file; do
    case "$file" in
        src/clock.rs|src/config/tests.rs) continue ;;
    esac

    production=$(awk '/^mod tests \{/{exit} {print}' "$file")
    if matches=$(rg -n "$pattern" <<<"$production"); then
        printf 'direct time access bypasses Clock in %s:\n%s\n' "$file" "$matches" >&2
        failed=1
    fi
done < <(rg --files src -g '*.rs')

if (( failed != 0 )); then
    printf 'route runtime clocks, sleeps, and deadlines through src/clock.rs\n' >&2
    exit 1
fi

printf 'time abstraction check passed\n'
