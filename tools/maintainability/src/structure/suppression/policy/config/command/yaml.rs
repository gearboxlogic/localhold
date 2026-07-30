use std::path::Path;

enum RunValue {
    Inline(String),
    Block { indentation: usize, folded: bool },
}

pub(super) fn run_commands(path: &str, source: &str) -> Vec<String> {
    if !is_github_yaml(path) {
        return Vec::new();
    }
    let lines = source.lines().collect::<Vec<_>>();
    let mut commands = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let Some(value) = run_value(lines[index]) else {
            index += 1;
            continue;
        };
        let (header_indentation, folded) = match value {
            RunValue::Inline(command) => {
                commands.push(command);
                index += 1;
                continue;
            }
            RunValue::Block { indentation, folded } => (indentation, folded),
        };
        index += 1;
        let mut command = String::new();
        let mut previous_was_content = false;
        while index < lines.len() {
            let line = lines[index];
            if line.trim().is_empty() {
                command.push('\n');
                previous_was_content = false;
                index += 1;
                continue;
            }
            let indentation = leading_spaces(line);
            if indentation <= header_indentation {
                break;
            }
            if previous_was_content {
                command.push(block_separator(folded));
            }
            command.push_str(&line[indentation..]);
            previous_was_content = true;
            index += 1;
        }
        commands.push(command);
    }
    commands
}

const fn block_separator(folded: bool) -> char {
    if folded { ' ' } else { '\n' }
}

fn run_value(line: &str) -> Option<RunValue> {
    let indentation = leading_spaces(line);
    let mut content = &line[indentation..];
    if let Some(rest) = content.strip_prefix("- ") {
        content = rest.trim_start();
    }
    let (key, value) = content.split_once(':')?;
    if !matches!(key.trim(), "run" | "'run'" | "\"run\"") {
        return None;
    }
    let value = value.trim_start();
    if !matches!(value.as_bytes().first(), Some(b'>' | b'|')) {
        return (!value.is_empty()).then(|| RunValue::Inline(value.to_owned()));
    }
    let indicator_length = value.find(char::is_whitespace).unwrap_or(value.len());
    let (indicator, trailing) = value.split_at(indicator_length);
    (indicator[1..].chars().all(|character| character.is_ascii_digit() || matches!(character, '+' | '-')) && (trailing.trim().is_empty() || trailing.trim_start().starts_with('#')))
        .then(|| RunValue::Block {
            indentation,
            folded: indicator.starts_with('>'),
        })
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|byte| *byte == b' ').count()
}

fn is_github_yaml(path: &str) -> bool {
    let path = Path::new(path);
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "yml" | "yaml"))
        && (path.starts_with(".github/workflows") || path.starts_with(".github/actions"))
}
