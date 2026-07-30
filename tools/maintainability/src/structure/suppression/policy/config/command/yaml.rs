use std::path::Path;

pub(super) fn folded_run_commands(path: &str, source: &str) -> Vec<String> {
    if !is_github_yaml(path) {
        return Vec::new();
    }
    let lines = source.lines().collect::<Vec<_>>();
    let mut commands = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let Some(header_indentation) = folded_run_header(lines[index]) else {
            index += 1;
            continue;
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
                command.push(' ');
            }
            command.push_str(&line[indentation..]);
            previous_was_content = true;
            index += 1;
        }
        commands.push(command);
    }
    commands
}

fn folded_run_header(line: &str) -> Option<usize> {
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
    let indicator_length = value.find(char::is_whitespace).unwrap_or(value.len());
    let (indicator, trailing) = value.split_at(indicator_length);
    (indicator.starts_with('>')
        && indicator[1..].chars().all(|character| character.is_ascii_digit() || matches!(character, '+' | '-'))
        && (trailing.trim().is_empty() || trailing.trim_start().starts_with('#')))
    .then_some(indentation)
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
