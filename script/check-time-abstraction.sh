#!/usr/bin/env bash
set -euo pipefail

pattern='(chrono::)?Utc::now\(|SystemTime::now\(|(tokio::time::|std::time::)?Instant::now\(|tokio::time::(sleep|sleep_until|interval|timeout)\(|(std::)?thread::sleep\('
failed=0

while IFS= read -r file; do
    case "$file" in
        src/clock.rs|src/config/tests.rs) continue ;;
    esac

    production=
    in_tests=0
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$line" == 'mod tests {' ]]; then
            if (( in_tests != 0 )); then
                in_tests=2
                break
            fi
            in_tests=1
            continue
        fi
        if (( in_tests == 1 )) && [[ "$line" == '}' ]]; then
            in_tests=0
            continue
        fi
        if (( in_tests == 0 )); then
            printf -v production '%s%s\n' "$production" "$line"
        fi
    done <"$file"
    if (( in_tests != 0 )); then
        printf 'time abstraction check could not identify the inline test-module boundary in %s\n' "$file" >&2
        failed=1
        continue
    fi
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
