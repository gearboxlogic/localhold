use super::{command_word_index, tokens};

pub(super) fn has_opaque_evaluation(path: &str, source: &str, source_is_reviewed: bool) -> bool {
    if tokens::has_opaque_heredoc_delimiter(source) {
        return true;
    }
    let expansion_source = tokens::shell_expansion_source(source);
    let expansion_source = expansion_source.replace("\\\r\n", "").replace("\\\n", "");
    has_legacy_arithmetic_expansion(&expansion_source)
        || expressions(&expansion_source)
            .iter()
            .any(|expression| expression_is_opaque(path, expression, source_is_reviewed))
        || parameter_expansions(&expansion_source)
            .iter()
            .any(|parameter| parameter_is_opaque(path, parameter, source_is_reviewed))
        || command_is_opaque(path, source, source_is_reviewed)
}

fn has_legacy_arithmetic_expansion(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    for index in 0..bytes.len().saturating_sub(1) {
        let byte = bytes[index];
        if comment {
            comment = byte != b'\n';
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if byte == b'\\' && quote != Some(b'\'') {
            escaped = true;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = if quote == Some(byte) { None } else { quote.or(Some(byte)) };
            continue;
        }
        if quote.is_none() && shell_comment_opener(bytes, index) {
            comment = true;
            continue;
        }
        if quote != Some(b'\'') && bytes[index..].starts_with(b"$[") {
            return true;
        }
    }
    false
}

fn shell_comment_opener(bytes: &[u8], index: usize) -> bool {
    bytes[index] == b'#'
        && index
            .checked_sub(1)
            .is_none_or(|previous| bytes[previous].is_ascii_whitespace() || matches!(bytes[previous], b';' | b'&' | b'|' | b'(' | b')'))
}

pub(super) fn reviewed_associative_subscript(path: &str, source_is_reviewed: bool, name: &str, subscript: &str) -> bool {
    let subscript = subscript.trim_matches(['\'', '"']);
    source_is_reviewed && path == "script/check-maintainability-bootstrap.sh" && matches!((name, subscript), ("expected_paths" | "expected_index_entries", "$relative_path"))
}

pub(super) fn literal_subscript(subscript: &str) -> bool {
    let subscript = subscript.trim_matches(['\'', '"']).trim();
    matches!(subscript, "@" | "*") || !subscript.is_empty() && subscript.bytes().all(|byte| byte.is_ascii_digit())
}

fn expressions(source: &str) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut expressions = Vec::new();
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    while index + 1 < bytes.len() {
        let byte = bytes[index];
        if comment {
            if byte == b'\n' {
                comment = false;
            }
            index += 1;
            continue;
        }
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if byte == b'\\' && quote != Some(b'\'') {
            escaped = true;
            index += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = if quote == Some(byte) {
                None
            } else if quote.is_none() {
                Some(byte)
            } else {
                quote
            };
            index += 1;
            continue;
        }
        if quote.is_none() && byte == b'#' && index.checked_sub(1).is_none_or(|previous| bytes[previous].is_ascii_whitespace()) {
            comment = true;
            index += 1;
            continue;
        }
        let arithmetic_start = quote != Some(b'\'') && bytes[index..].starts_with(b"$((") || quote.is_none() && bytes[index..].starts_with(b"((");
        if !arithmetic_start {
            index += 1;
            continue;
        }
        let opener = if bytes[index] == b'$' { 3 } else { 2 };
        let start = index + opener;
        let Some(end) = expression_end(bytes, start) else {
            return expressions;
        };
        expressions.push(&source[start..end - 1]);
        index = end + 1;
    }
    expressions
}

fn expression_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 2_usize;
    for (cursor, byte) in bytes.iter().copied().enumerate().skip(start) {
        match byte {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return Some(cursor);
        }
    }
    None
}

fn parameter_expansions(source: &str) -> Vec<&str> {
    let bytes = source.as_bytes();
    let mut expansions = Vec::new();
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    while index + 2 < bytes.len() {
        let byte = bytes[index];
        if comment {
            comment = byte != b'\n';
        } else if escaped {
            escaped = false;
        } else if byte == b'\\' && quote != Some(b'\'') {
            escaped = true;
        } else if matches!(byte, b'\'' | b'"') {
            quote = if quote == Some(byte) {
                None
            } else if quote.is_none() {
                Some(byte)
            } else {
                quote
            };
        } else if quote.is_none() && byte == b'#' && index.checked_sub(1).is_none_or(|previous| bytes[previous].is_ascii_whitespace()) {
            comment = true;
        } else if quote != Some(b'\'')
            && bytes[index..].starts_with(b"${")
            && let Some(end) = delimited_expression_end(bytes, index + 2, b'{', b'}')
        {
            expansions.push(&source[index + 2..end]);
        }
        index += 1;
    }
    expansions
}

fn delimited_expression_end(bytes: &[u8], start: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 1_usize;
    for (cursor, byte) in bytes.iter().copied().enumerate().skip(start) {
        if byte == open {
            depth += 1;
        } else if byte == close {
            depth -= 1;
            if depth == 0 {
                return Some(cursor);
            }
        }
    }
    None
}

fn parameter_is_opaque(path: &str, parameter: &str, source_is_reviewed: bool) -> bool {
    let parameter = parameter.strip_prefix(['#', '!']).unwrap_or(parameter);
    let Some((name, mut index)) = leading_identifier(parameter) else {
        return special_parameter_prompt_transform(parameter);
    };
    if parameter.as_bytes().get(index) == Some(&b'[') {
        let start = index + 1;
        let Some(end) = delimited_expression_end(parameter.as_bytes(), start, b'[', b']') else {
            return true;
        };
        let subscript = &parameter[start..end];
        if !literal_subscript(subscript) && !reviewed_associative_subscript(path, source_is_reviewed, name, subscript) {
            return true;
        }
        index = end + 1;
    }
    let remainder = &parameter[index..];
    if remainder == "@P" {
        return true;
    }
    let Some(slice) = remainder.strip_prefix(':') else {
        return false;
    };
    if slice.starts_with(['-', '=', '?', '+']) {
        return false;
    }
    slice
        .split(':')
        .filter(|expression| !expression.trim().is_empty())
        .any(|expression| expression_is_opaque(path, expression, source_is_reviewed))
}

fn special_parameter_prompt_transform(parameter: &str) -> bool {
    let bytes = parameter.as_bytes();
    let index = if bytes.first().is_some_and(u8::is_ascii_digit) {
        bytes.iter().position(|byte| !byte.is_ascii_digit()).unwrap_or(bytes.len())
    } else if bytes.first().is_some_and(|byte| matches!(byte, b'*' | b'@' | b'#' | b'?' | b'-' | b'$' | b'!')) {
        1
    } else {
        return false;
    };
    &parameter[index..] == "@P"
}

fn leading_identifier(source: &str) -> Option<(&str, usize)> {
    let bytes = source.as_bytes();
    if !bytes.first().is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_') {
        return None;
    }
    let end = bytes.iter().position(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_')).unwrap_or(bytes.len());
    Some((&source[..end], end))
}

fn command_is_opaque(path: &str, source: &str, source_is_reviewed: bool) -> bool {
    tokens::source_command_tokens(source).iter().any(|command| {
        let Some(index) = command_word_index(command) else {
            return false;
        };
        let command_word = command[index].trim_matches(['(', ')', '{', '}']);
        if command_word.eq_ignore_ascii_case("let")
            && command[index + 1..]
                .iter()
                .any(|expression| expression_is_opaque(path, expression.trim_matches(';'), source_is_reviewed))
        {
            return true;
        }
        if matches!(command_word, "test" | "[")
            && command[index + 1..]
                .windows(2)
                .any(|pair| pair[0] == "-v" && variable_target_is_opaque(path, source, &pair[1], source_is_reviewed))
        {
            return true;
        }
        let Some(conditional) = command.iter().position(|word| word == "[[") else {
            return false;
        };
        let arguments = &command[conditional + 1..];
        arguments
            .windows(2)
            .any(|pair| pair[0] == "-v" && variable_target_is_opaque(path, source, &pair[1], source_is_reviewed))
            || arguments.windows(3).any(|triple| {
                matches!(triple[1].as_str(), "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge")
                    && (expression_is_opaque(path, &triple[0], source_is_reviewed) || expression_is_opaque(path, &triple[2], source_is_reviewed))
            })
    })
}

fn variable_target_is_opaque(path: &str, source: &str, target: &str, source_is_reviewed: bool) -> bool {
    let target = target.trim_matches(['\'', '"']);
    let Some((name, index)) = leading_identifier(target) else {
        return target.contains('$') && !reviewed_test_environment_target(path, source, target, source_is_reviewed);
    };
    let Some(subscript) = target[index..].strip_prefix('[').and_then(|subscript| subscript.strip_suffix(']')) else {
        return false;
    };
    !literal_subscript(subscript) && !reviewed_associative_subscript(path, source_is_reviewed, name, subscript)
}

fn reviewed_test_environment_target(path: &str, source: &str, target: &str, source_is_reviewed: bool) -> bool {
    source_is_reviewed
        && path == "script/run-maintainability-gate.sh"
        && target == "$name"
        && source.contains("verify_test_environment() {\n    local entry\n    local name\n    local uppercase\n    while IFS= read -r -d '' entry; do")
        && source.contains("    done < <(\"$env_command\" -0)\n    for name in BASH_ENV ENV CCC_OVERRIDE_OPTIONS CL COMPILER_PATH")
        && source.contains("CARGO_TARGET_TEST_LINKER CARGO_TARGET_TEST_RUNNER; do\n        if [[ -v $name ]]; then")
}

fn expression_is_opaque(path: &str, expression: &str, source_is_reviewed: bool) -> bool {
    if reviewed_expression(path, expression, source_is_reviewed) {
        return false;
    }
    let bytes = expression.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'$' && special_parameter_follows(bytes, index + 1) {
            return true;
        }
        if !(bytes[index].is_ascii_alphabetic() || bytes[index] == b'_') {
            index += 1;
            continue;
        }
        let name_start = index;
        while bytes.get(index).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_') {
            index += 1;
        }
        let name = &expression[name_start..index];
        if bytes.get(index) != Some(&b'[') {
            return true;
        }
        let start = index + 1;
        let Some(end) = delimited_expression_end(bytes, start, b'[', b']') else {
            return true;
        };
        let subscript = &expression[start..end];
        if !literal_subscript(subscript) && !reviewed_associative_subscript(path, source_is_reviewed, name, subscript) {
            return true;
        }
        index = end + 1;
    }
    false
}

fn special_parameter_follows(bytes: &[u8], mut index: usize) -> bool {
    let braced = bytes.get(index) == Some(&b'{');
    if braced {
        index += 1;
    }
    if braced && bytes.get(index) == Some(&b'#') && bytes.get(index + 1).is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_') {
        return false;
    }
    bytes
        .get(index)
        .is_some_and(|byte| byte.is_ascii_digit() || matches!(byte, b'*' | b'@' | b'#' | b'?' | b'-' | b'$' | b'!'))
}

fn reviewed_expression(path: &str, expression: &str, source_is_reviewed: bool) -> bool {
    if !source_is_reviewed {
        return false;
    }
    let expression = expression.trim();
    matches!(
        (path, expression),
        ("script/dep-audit.sh", "failed != 0")
            | ("script/install.sh", "$# > 0" | "$# >= 2")
            | (
                "script/check-maintainability-bootstrap.sh",
                "expected_count += 1"
                    | "expected_count == 0"
                    | "observed_count += 1"
                    | "observed_count != expected_count"
                    | "indexed_count += 1"
                    | "indexed_count != expected_count"
                    | "status == 0"
            )
            | ("script/claude-review.sh", "status == 0")
            | ("script/run-maintainability-gate.sh", "$# != 1")
            | ("script/test-postgres-smoke.sh", "$status")
            | ("script/tests/test_claude_review.sh", "status != 23" | "status != 143")
            | (
                "script/tests/test_maintainability_bootstrap.sh",
                "guard_count != 2" | "loader_guard_count != 2" | "SECONDS + 300" | "SECONDS < deadline" | "snapshot_status != 0"
            )
    )
}

#[cfg(test)]
mod tests {
    use super::has_opaque_evaluation;

    #[test]
    fn arithmetic_subscripts_require_literal_or_profiled_targets() {
        for source in [
            "(( a[key]++ ))",
            "(( a[key] = 1 ))",
            "$(( a[key] + 1 ))",
            "let 'a[key]++'",
            ": \"${a[key]}\"",
            ": \"${a[$key]}\"",
            ": \"${#a[$key]}\"",
            ": \"${!a[key]}\"",
            concat!(": \"$", "{value:key}\""),
            "[[ -v a[key] ]]",
            "test -v a[key]",
            "[ -v a[key] ]",
            "name=a[key]; builtin test -v \"$name\"",
            "[[ a[key] -eq 0 ]]",
            "(( key ))",
            "let key",
            "[[ key -eq 0 ]]",
            "(( $1 ))",
            "(( ${1} ))",
            "let '$?'",
            "[[ $@ -eq 0 ]]",
            ": \"${value:$#:2}\"",
            ": \"${payload@P}\"",
            ": \"${1@P}\"",
        ] {
            assert!(has_opaque_evaluation("script/check.sh", source, false), "{source}");
        }
        for source in [
            "(( a[0]++ ))",
            "(( ${#values[@]} != 1 ))",
            ": \"${a[0]}\"",
            ": \"${!a[@]}\"",
            ": \"${value:1:2}\"",
            "[[ -v a[0] ]]",
            "test -v a[0]",
            "[ -v a[0] ]",
            "[[ a[0] -eq 0 ]]",
            "printf '%s' '(( a[key]++ ))'",
            "printf '%s' \"(( a[key]++ ))\"",
            "cat <<'LITERAL'\n$((a[key]))\n${payload@P}\nLITERAL\n",
            "(( 1 + 2 ))",
            "[[ 1 -eq 1 ]]",
            ": \"${value:1:2}\"",
        ] {
            assert!(!has_opaque_evaluation("script/check.sh", source, false), "{source}");
        }
        assert!(!has_opaque_evaluation(
            "script/check-maintainability-bootstrap.sh",
            "((expected_count += 1)); : \"${expected_paths[\"$relative_path\"]+present}\"",
            true
        ));
        assert!(has_opaque_evaluation("script/check-maintainability-bootstrap.sh", "((unreviewed_count += 1))", true));
        assert!(has_opaque_evaluation("script/check-publication-hygiene.sh", "(( failed != 0 ))", true));
        let reviewed_environment_scan = r#"verify_test_environment() {
    local entry
    local name
    local uppercase
    while IFS= read -r -d '' entry; do
        :
    done < <("$env_command" -0)
    for name in BASH_ENV ENV CCC_OVERRIDE_OPTIONS CL COMPILER_PATH values \
CARGO_TARGET_TEST_LINKER CARGO_TARGET_TEST_RUNNER; do
        if [[ -v $name ]]; then
            :
        fi
    done
}"#;
        assert!(!has_opaque_evaluation("script/run-maintainability-gate.sh", reviewed_environment_scan, true));
        assert!(has_opaque_evaluation(
            "script/run-maintainability-gate.sh",
            &reviewed_environment_scan.replace("local uppercase", "local changed"),
            true
        ));
    }

    #[test]
    fn continued_openers_and_expanding_heredocs_remain_visible() {
        for source in [
            "$\\\n((payload))",
            "(\\\n(payload))",
            ": \"$\\\n{value:$1}\"",
            ": \"${payload@\\\nP}\"",
            "cat <<ACTIVE\n'$((payload))' # shell quotes are data\nACTIVE\n",
        ] {
            assert!(has_opaque_evaluation("script/check.sh", source, false), "{source}");
        }
        assert!(!has_opaque_evaluation("script/check.sh", "cat <<'LITERAL'\n\\\nLITERAL\n(( 1 + 2 ))\n", false));
    }

    #[test]
    fn legacy_arithmetic_expansions_fail_closed_without_matching_inert_text() {
        for source in [": $[payload]", ": \"$[payload + 1]\"", ": ${value:-$[payload]}"] {
            assert!(has_opaque_evaluation("script/check.sh", source, false), "{source}");
        }
        for source in [
            "printf '%s' '$[payload]'",
            "printf '%s' \"\\$[payload]\"",
            "printf ok;# $[payload]",
            "cat <<'LITERAL'\n$[payload]\nLITERAL\n",
        ] {
            assert!(!has_opaque_evaluation("script/check.sh", source, false), "{source}");
        }
        for source in [
            "cat <<$'EOF' >/dev/null\nsafe\nEOF\n: $[payload]\n",
            "cat <<$'E\\x4fF' >/dev/null\nsafe\nEOF\n: $[payload]\n",
            "cat <<$\"EOF\" >/dev/null\nsafe\nEOF\n: $[payload]\n",
        ] {
            assert!(has_opaque_evaluation("script/check.sh", source, false), "{source}");
        }
    }
}
