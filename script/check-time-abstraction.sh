#!/usr/bin/env bash
set -euo pipefail

pattern='(chrono::)?Utc::now\(|SystemTime::now\(|(tokio::time::|std::time::)?Instant::now\(|tokio::time::(sleep|sleep_until|interval|timeout)\(|(std::)?thread::sleep\('
failed=0
brace_delta=0
block_comment_depth=0
normal_string=0
raw_string=0
raw_hashes=

rust_brace_delta() {
    local text=$1
    local index=0
    local length=${#text}
    local character next pair raw_index cursor closing
    brace_delta=0

    while (( index < length )); do
        character=${text:index:1}
        next=${text:index+1:1}
        pair=${text:index:2}

        if (( block_comment_depth > 0 )); then
            if [[ $pair == '/*' ]]; then
                block_comment_depth=$(( block_comment_depth + 1 ))
                index=$(( index + 2 ))
            elif [[ $pair == '*/' ]]; then
                block_comment_depth=$(( block_comment_depth - 1 ))
                index=$(( index + 2 ))
            else
                index=$(( index + 1 ))
            fi
            continue
        fi
        if (( raw_string != 0 )); then
            closing="\"$raw_hashes"
            if [[ ${text:index:${#closing}} == "$closing" ]]; then
                raw_string=0
                index=$(( index + ${#closing} ))
            else
                index=$(( index + 1 ))
            fi
            continue
        fi
        if (( normal_string != 0 )); then
            if [[ $character == '\\' ]]; then
                index=$(( index + 2 ))
            else
                if [[ $character == '"' ]]; then
                    normal_string=0
                fi
                index=$(( index + 1 ))
            fi
            continue
        fi
        if [[ $pair == '//' ]]; then
            break
        fi
        if [[ $pair == '/*' ]]; then
            block_comment_depth=1
            index=$(( index + 2 ))
            continue
        fi

        raw_index=-1
        if [[ $character == r ]]; then
            raw_index=$index
        elif [[ $character == b || $character == c ]] && [[ $next == r ]]; then
            raw_index=$(( index + 1 ))
        fi
        if (( raw_index >= 0 )); then
            cursor=$(( raw_index + 1 ))
            raw_hashes=
            while [[ ${text:cursor:1} == '#' ]]; do
                raw_hashes+='#'
                cursor=$(( cursor + 1 ))
            done
            if [[ ${text:cursor:1} == '"' ]]; then
                raw_string=1
                index=$(( cursor + 1 ))
                continue
            fi
        fi

        if [[ $character == '"' ]] || { [[ $character == b || $character == c ]] && [[ $next == '"' ]]; }; then
            normal_string=1
            if [[ $character == '"' ]]; then
                index=$(( index + 1 ))
            else
                index=$(( index + 2 ))
            fi
            continue
        fi

        cursor=$index
        if [[ $character == b && $next == "'" ]]; then
            cursor=$(( index + 1 ))
        fi
        if [[ ${text:cursor:1} == "'" ]]; then
            closing=$(( cursor + 2 ))
            if [[ ${text:cursor+1:1} == '\\' ]]; then
                if [[ ${text:cursor+2:2} == 'u{' ]]; then
                    closing=$(( cursor + 4 ))
                    while (( closing < length )) && [[ ${text:closing:1} != '}' ]]; do
                        closing=$(( closing + 1 ))
                    done
                    closing=$(( closing + 1 ))
                elif [[ ${text:cursor+2:1} == x ]]; then
                    closing=$(( cursor + 5 ))
                else
                    closing=$(( cursor + 3 ))
                fi
            fi
            if [[ ${text:closing:1} == "'" ]]; then
                index=$(( closing + 1 ))
                continue
            fi
        fi

        if [[ $character == '{' ]]; then
            brace_delta=$(( brace_delta + 1 ))
        elif [[ $character == '}' ]]; then
            brace_delta=$(( brace_delta - 1 ))
        fi
        index=$(( index + 1 ))
    done
}

source_files=()
while IFS= read -r -d '' file; do
    source_files+=("$file")
done < <(/usr/bin/find src \( -type f -o -type l \) -name '*.rs' -print0)
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
    test_depth=0
    line_number=0
    block_comment_depth=0
    normal_string=0
    raw_string=0
    raw_hashes=
    while IFS= read -r line || [[ -n "$line" ]]; do
        (( ++line_number ))
        if (( in_tests == 1 )); then
            rust_brace_delta "$line"
            test_depth=$(( test_depth + brace_delta ))
            if (( test_depth < 0 )); then
                in_tests=2
                break
            fi
            if (( test_depth == 0 )); then
                in_tests=0
            fi
            continue
        fi
        if (( block_comment_depth == 0 && normal_string == 0 && raw_string == 0 )) && [[ "$line" == 'mod tests {' ]]; then
            in_tests=1
            test_depth=1
            continue
        fi
        if [[ $line =~ $pattern ]]; then
            printf -v matches '%s%s:%s\n' "$matches" "$line_number" "$line"
        fi
        if (( block_comment_depth > 0 || normal_string != 0 || raw_string != 0 )) || [[ $line == *'/*'* || $line == *'"'* ]]; then
            rust_brace_delta "$line"
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
