#!/usr/bin/env bash
set -euo pipefail

pattern='(chrono::)?Utc::now\(|SystemTime::now\(|(tokio::time::|std::time::)?Instant::now\(|tokio::time::(sleep|sleep_until|interval|timeout)\(|(std::)?thread::sleep\('
failed=0

shopt -s dotglob globstar nullglob
source_files=(src/**/*.rs)
if (( ${#source_files[@]} == 0 )); then
    printf 'time abstraction check found no Rust sources\n' >&2
    exit 1
fi

for file in "${source_files[@]}"; do
    case "$file" in
        src/clock.rs|src/config/tests.rs) continue ;;
    esac

    if [[ ! -f $file || -L $file ]]; then
        printf 'time abstraction check requires a regular non-symlink Rust source: %s\n' "$file" >&2
        failed=1
        continue
    fi
    matches=
    in_tests=0
    line_number=0
    while IFS= read -r line || [[ -n "$line" ]]; do
        (( ++line_number ))
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
        if (( in_tests == 0 )) && [[ $line =~ $pattern ]]; then
            printf -v matches '%s%s:%s\n' "$matches" "$line_number" "$line"
        fi
    done <"$file"
    if (( in_tests != 0 )); then
        printf 'time abstraction check could not identify the inline test-module boundary in %s\n' "$file" >&2
        failed=1
        continue
    fi
    if [[ -n $matches ]]; then
        printf 'direct time access bypasses Clock in %s:\n%s' "$file" "$matches" >&2
        failed=1
    fi
done

if (( failed != 0 )); then
    printf 'route runtime clocks, sleeps, and deadlines through src/clock.rs\n' >&2
    exit 1
fi

printf 'time abstraction check passed\n'
