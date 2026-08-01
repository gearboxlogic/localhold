use super::{is_environment_assignment, is_shell_command_prefix, tokens};

pub(super) fn contains_command(source: &str) -> bool {
    let source = tokens::without_noncommand_shell_data(source);
    tokens::source_command_tokens(&source).iter().any(|command| contains_in_tokens(command))
}

fn contains_in_tokens(tokens: &[String]) -> bool {
    let mut index = 0;
    while let Some(raw_word) = tokens.get(index) {
        let word = raw_word.trim_matches(['(', ')', '{', '}']);
        if matches!(word, "." | "source") {
            return tokens.get(index + 1).is_some();
        }
        if word.is_empty() || matches!(word, "nocorrect" | "noglob" | "time") || is_shell_command_prefix(word) || is_environment_assignment(word) {
            index += 1;
            continue;
        }
        if let Some(width) = redirection_width(raw_word) {
            index += width;
            continue;
        }
        if let Some(width) = function_header_width(tokens, index) {
            index += width;
            continue;
        }
        if index == 0 && raw_word.ends_with(')') {
            index += 1;
            continue;
        }
        if word == "case"
            && let Some(body) = tokens[index + 1..].iter().position(|candidate| candidate.ends_with(')'))
        {
            index += body + 2;
            continue;
        }
        return false;
    }
    false
}

fn redirection_width(word: &str) -> Option<usize> {
    let mut suffix = word.trim_start_matches(|character: char| character.is_ascii_digit());
    if let Some(remainder) = suffix.strip_prefix('{').and_then(|value| value.split_once('}').map(|(_, remainder)| remainder)) {
        suffix = remainder;
    }
    for operator in ["<<<", "<<", ">>", "<>", ">|", "<&", ">&", "<", ">"] {
        if suffix == operator {
            return Some(2);
        }
        if suffix.starts_with(operator) {
            return Some(1);
        }
    }
    None
}

fn function_header_width(tokens: &[String], index: usize) -> Option<usize> {
    let word = tokens.get(index)?;
    if word.strip_suffix("(){").is_some_and(valid_name) {
        return Some(1);
    }
    if word.strip_suffix("()").is_some_and(valid_name) && tokens.get(index + 1).is_some_and(|next| next == "{") {
        return Some(2);
    }
    if valid_name(word) && tokens.get(index + 1).is_some_and(|next| matches!(next.as_str(), "(){" | "()")) {
        return if tokens[index + 1] == "(){" {
            Some(2)
        } else if tokens.get(index + 2).is_some_and(|next| next == "{") {
            Some(3)
        } else {
            None
        };
    }
    if word == "function" {
        let name = tokens.get(index + 1)?;
        let name = name.strip_suffix("()").unwrap_or(name);
        if valid_name(name) {
            if tokens.get(index + 2).is_some_and(|next| matches!(next.as_str(), "{" | "(){")) {
                return Some(3);
            }
            if tokens.get(index + 2).is_some_and(|next| next == "()") && tokens.get(index + 3).is_some_and(|next| next == "{") {
                return Some(4);
            }
        }
    }
    None
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.as_bytes()[0].is_ascii_digit() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::contains_command;

    #[test]
    fn source_builtins_are_found_behind_shell_grammar() {
        for source in [
            "source quality/lint.txt",
            ". quality/lint.txt",
            "time -p source quality/lint.txt",
            ">quality/log source quality/lint.txt",
            "> quality/log . quality/lint.txt",
            "load() { source quality/lint.txt; }; load",
            "load () { source quality/lint.txt; }; load",
            "function load { . quality/lint.txt; }; load",
            "function load () { . quality/lint.txt; }; load",
            "case yes in yes) source quality/lint.txt;; esac",
            "case yes in no|yes) source quality/lint.txt;; esac",
            "case yes in no) :;; yes) source quality/lint.txt;; esac",
            "case yes in\n  yes) . quality/lint.txt ;;\nesac",
            "noglob source quality/lint.txt",
            "nocorrect . quality/lint.txt",
        ] {
            assert!(contains_command(source), "{source}");
        }
    }

    #[test]
    fn inert_source_words_are_not_commands() {
        for source in [
            "printf '%s\\n' source quality/lint.txt",
            "echo '. quality/lint.txt'",
            "case yes in source) printf ok;; esac",
            "case yes in yes) printf '%s' source quality/lint.txt;; esac",
            "case 'in ) ;; source' in yes) printf ok;; esac",
            "report() { printf '%s' source; }; report",
        ] {
            assert!(!contains_command(source), "{source}");
        }
    }
}
