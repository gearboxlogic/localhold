use std::path::Path;

use anyhow::{Result, bail};

enum RunValue {
    Inline(String),
    Block { indentation: usize, folded: bool },
}

pub(super) fn validate_execution_metadata(path: &str, source: &str) -> Result<()> {
    if !is_github_yaml(path) {
        return Ok(());
    }
    let mut block_indentation = None;
    for line in source.lines() {
        let indentation = leading_spaces(line);
        if block_indentation.is_some_and(|header| line.trim().is_empty() || indentation > header) {
            continue;
        }
        block_indentation = None;
        let Some((key, value)) = yaml_key_value(line) else {
            continue;
        };
        let value = value.trim_start();
        if starts_yaml_reference(value) {
            bail!("checked-in GitHub YAML {path:?} uses unsupported anchors or aliases");
        }
        if key == "shell" {
            let shell = literal_scalar(value).ok_or_else(|| anyhow::anyhow!("checked-in GitHub YAML {path:?} uses an unsupported shell template"))?;
            if !matches!(shell.as_str(), "bash" | "sh" | "pwsh" | "powershell" | "cmd" | "python") {
                bail!("checked-in GitHub YAML {path:?} uses an unsupported shell template");
            }
        } else if is_block_scalar(value) {
            block_indentation = Some(indentation);
        }
    }
    Ok(())
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

pub(super) fn yaml_key_value(line: &str) -> Option<(&str, &str)> {
    let mut content = line.trim_start();
    if let Some(rest) = content.strip_prefix("- ") {
        content = rest.trim_start();
    }
    let (key, value) = content.split_once(':')?;
    Some((key.trim_matches(['\'', '"']).trim(), value))
}

fn starts_yaml_reference(value: &str) -> bool {
    matches!(value.as_bytes(), [b'&' | b'*', name, ..] if name.is_ascii_alphanumeric() || matches!(*name, b'_' | b'-'))
}

pub(super) fn literal_scalar(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || starts_yaml_reference(value) || value.starts_with(['{', '[', '|', '>']) {
        return None;
    }
    if let Some(value) = value.strip_prefix('\'') {
        let (value, trailing) = value.rsplit_once('\'')?;
        return (trailing.trim().is_empty() || trailing.trim_start().starts_with('#')).then(|| value.replace("''", "'"));
    }
    if let Some(value) = value.strip_prefix('"') {
        let (value, trailing) = value.rsplit_once('"')?;
        return (!value.contains('\\') && (trailing.trim().is_empty() || trailing.trim_start().starts_with('#'))).then(|| value.to_owned());
    }
    Some(value.split(" #").next().unwrap_or(value).trim().to_owned())
}

fn is_block_scalar(value: &str) -> bool {
    let indicator_length = value.find(char::is_whitespace).unwrap_or(value.len());
    let (indicator, trailing) = value.split_at(indicator_length);
    matches!(indicator.as_bytes().first(), Some(b'>' | b'|'))
        && indicator[1..].chars().all(|character| character.is_ascii_digit() || matches!(character, '+' | '-'))
        && (trailing.trim().is_empty() || trailing.trim_start().starts_with('#'))
}

const fn block_separator(folded: bool) -> char {
    if folded { ' ' } else { '\n' }
}

fn run_value(line: &str) -> Option<RunValue> {
    let indentation = leading_spaces(line);
    let (key, value) = yaml_key_value(line)?;
    if key != "run" {
        return None;
    }
    let value = value.trim_start();
    if !is_block_scalar(value) {
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

pub(super) fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|byte| *byte == b' ').count()
}

fn is_github_yaml(path: &str) -> bool {
    let path = Path::new(path);
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "yml" | "yaml"))
        && (path.starts_with(".github/workflows")
            || matches!(
                path.file_name().and_then(|name| name.to_str()).map(str::to_ascii_lowercase).as_deref(),
                Some("action.yml" | "action.yaml")
            ))
}
