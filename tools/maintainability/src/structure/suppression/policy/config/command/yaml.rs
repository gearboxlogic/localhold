use std::path::Path;

use anyhow::{Result, bail};

mod governed;

enum RunValue {
    Inline(String),
    Block { indentation: usize, folded: bool },
}

struct BlockScalar {
    indentation: usize,
    rejects_expressions: bool,
}

struct RunCommand {
    source: String,
    line_index: usize,
    indentation: usize,
}

pub(super) fn validate_execution_metadata(path: &str, source: &str) -> Result<()> {
    if !is_github_yaml(path) {
        return Ok(());
    }
    governed::validate(path, source)?;
    let mut block_indentation: Option<BlockScalar> = None;
    let mut inline_run_indentation = None;
    let lines = source.lines().collect::<Vec<_>>();
    for (line_index, line) in lines.iter().copied().enumerate() {
        let indentation = leading_spaces(line);
        if let Some(header) = inline_run_indentation {
            if line.trim().is_empty() {
                continue;
            }
            if indentation > header {
                bail!("checked-in GitHub YAML {path:?} uses an unsupported multiline inline run scalar");
            }
            inline_run_indentation = None;
        }
        if let Some(block) = &block_indentation
            && (line.trim().is_empty() || indentation > block.indentation)
        {
            if block.rejects_expressions && contains_github_expression(line) {
                bail!("checked-in GitHub YAML {path:?} uses an unsupported dynamic run expression");
            }
            continue;
        }
        block_indentation = None;
        if contains_working_directory_key(line) && !is_reviewed_working_directory(path, line) {
            bail!("checked-in GitHub YAML {path:?} uses an unsupported working-directory override");
        }
        if starts_unsupported_key_property(line) {
            bail!("checked-in GitHub YAML {path:?} uses unsupported anchors or aliases, node tags, or complex keys");
        }
        if starts_flow_collection(line) {
            bail!("checked-in GitHub YAML {path:?} uses an unsupported flow mapping or complex sequence");
        }
        if quoted_mapping_key_has_escape(line) {
            bail!("checked-in GitHub YAML {path:?} uses unsupported escapes in a quoted mapping key");
        }
        let Some((key, value)) = yaml_key_value(line) else {
            continue;
        };
        if key == "working-directory" && !is_reviewed_working_directory(path, line) {
            bail!("checked-in GitHub YAML {path:?} uses an unsupported working-directory override");
        }
        let value = value.trim_start();
        if key == "container" {
            bail!("checked-in GitHub YAML {path:?} uses an unsupported job container");
        }
        if starts_yaml_reference(value) {
            bail!("checked-in GitHub YAML {path:?} uses unsupported anchors or aliases");
        }
        if value.starts_with('{') || value.starts_with('[') && !is_simple_flow_sequence(value) {
            bail!("checked-in GitHub YAML {path:?} uses an unsupported flow mapping or complex sequence");
        }
        if key == "shell" {
            validate_shell(path, value, &lines, line_index)?;
        } else if key == "run" && value.starts_with('>') && is_block_scalar(value) {
            bail!("checked-in GitHub YAML {path:?} uses an unsupported folded run scalar");
        } else if is_block_scalar(value) {
            block_indentation = Some(BlockScalar {
                indentation,
                rejects_expressions: key == "run",
            });
        } else if key == "run" && !value.is_empty() {
            let run = literal_scalar(value).ok_or_else(|| anyhow::anyhow!("checked-in GitHub YAML {path:?} uses an unsupported inline run scalar"))?;
            if contains_github_expression(&run) {
                bail!("checked-in GitHub YAML {path:?} uses an unsupported dynamic run expression");
            }
            inline_run_indentation = Some(indentation);
        }
    }
    Ok(())
}

fn validate_shell(path: &str, value: &str, lines: &[&str], line_index: usize) -> Result<()> {
    let shell = literal_scalar(value).ok_or_else(|| anyhow::anyhow!("checked-in GitHub YAML {path:?} uses an unsupported shell template"))?;
    if !matches!(shell.as_str(), "bash" | "sh" | "pwsh" | "powershell" | "cmd") && !governed::is_startup_isolated_shell(&shell) {
        bail!("checked-in GitHub YAML {path:?} uses an unsupported shell template");
    }
    if matches!(shell.as_str(), "pwsh" | "powershell") && !shell_is_step_local(lines, line_index) {
        bail!("checked-in GitHub YAML {path:?} uses an unsupported default PowerShell shell; select PowerShell on each audited step");
    }
    Ok(())
}

fn shell_is_step_local(lines: &[&str], line_index: usize) -> bool {
    let shell_line = lines[line_index];
    if shell_line.trim_start().starts_with("- ") {
        return true;
    }
    let shell_indentation = leading_spaces(shell_line);
    for line in lines[..line_index].iter().rev().copied() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if leading_spaces(line) < shell_indentation {
            return line.trim_start().starts_with("- ");
        }
    }
    false
}

fn contains_github_expression(value: &str) -> bool {
    value.contains("${{")
}

fn contains_working_directory_key(line: &str) -> bool {
    let content = line.split_once(" #").map_or(line, |(content, _)| content);
    content.contains("working-directory:") || content.contains("working-directory':") || content.contains("working-directory\":")
}

fn is_reviewed_working_directory(path: &str, line: &str) -> bool {
    path == ".github/workflows/trusted-maintainability.yml" && line == "        working-directory: .candidate"
}

fn starts_unsupported_key_property(line: &str) -> bool {
    let content = yaml_node_content(line);
    starts_yaml_reference(content) || content.starts_with(['!', '?'])
}

fn starts_flow_collection(line: &str) -> bool {
    yaml_node_content(line).starts_with(['{', '['])
}

fn quoted_mapping_key_has_escape(line: &str) -> bool {
    let content = yaml_node_content(line);
    let Some(content) = content.strip_prefix('"') else {
        return false;
    };
    let mut escaped = false;
    let mut contained_escape = false;
    for (index, character) in content.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            contained_escape = true;
        } else if character == '"' {
            return contained_escape && content[index + character.len_utf8()..].trim_start().starts_with(':');
        }
    }
    false
}

fn yaml_node_content(line: &str) -> &str {
    let content = line.trim_start();
    content.strip_prefix("- ").map_or(content, str::trim_start)
}

fn is_simple_flow_sequence(value: &str) -> bool {
    let value = value.split_once(" #").map_or(value, |(value, _)| value).trim();
    let Some(contents) = value.strip_prefix('[').and_then(|value| value.strip_suffix(']')) else {
        return false;
    };
    contents.split(',').all(|item| {
        let item = item.trim();
        !item.is_empty() && item.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'))
    })
}

pub(super) fn run_commands(path: &str, source: &str) -> Vec<String> {
    if !is_github_yaml(path) {
        return Vec::new();
    }
    parsed_run_commands(source).into_iter().map(|command| command.source).collect()
}

pub(super) fn powershell_run_commands(path: &str, source: &str) -> Vec<String> {
    if !is_github_yaml(path) {
        return Vec::new();
    }
    let lines = source.lines().collect::<Vec<_>>();
    parsed_run_commands(source)
        .into_iter()
        .filter(|command| run_uses_powershell(&lines, command))
        .map(|command| command.source)
        .collect()
}

pub(super) fn non_powershell_run_commands(path: &str, source: &str) -> Vec<String> {
    if !is_github_yaml(path) {
        return Vec::new();
    }
    let lines = source.lines().collect::<Vec<_>>();
    parsed_run_commands(source)
        .into_iter()
        .filter(|command| !run_uses_powershell(&lines, command))
        .map(|command| command.source)
        .collect()
}

fn parsed_run_commands(source: &str) -> Vec<RunCommand> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut commands = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let Some(value) = run_value(lines[index]) else {
            index += 1;
            continue;
        };
        let line_index = index;
        let indentation = leading_spaces(lines[index]);
        let (header_indentation, folded) = match value {
            RunValue::Inline(command) => {
                commands.push(RunCommand {
                    source: command,
                    line_index,
                    indentation,
                });
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
        commands.push(RunCommand {
            source: command,
            line_index,
            indentation,
        });
    }
    commands
}

fn run_uses_powershell(lines: &[&str], command: &RunCommand) -> bool {
    let Some(step_start) = (0..=command.line_index).rev().find(|index| {
        let line = lines[*index];
        line.trim_start().starts_with("- ") && leading_spaces(line) <= command.indentation
    }) else {
        return false;
    };
    let step_indentation = leading_spaces(lines[step_start]);
    let property_indentation = if step_start == command.line_index {
        step_indentation.saturating_add(2)
    } else {
        command.indentation
    };
    let step_end = (step_start + 1..lines.len())
        .find(|index| {
            let line = lines[*index];
            if line.trim().is_empty() {
                return false;
            }
            let indentation = leading_spaces(line);
            indentation < step_indentation || indentation == step_indentation && line.trim_start().starts_with("- ")
        })
        .unwrap_or(lines.len());
    lines[step_start..step_end].iter().enumerate().any(|(offset, line)| {
        let absolute = step_start + offset;
        let property = absolute == step_start || leading_spaces(line) == property_indentation;
        property
            && yaml_key_value(line)
                .filter(|(key, _)| *key == "shell")
                .and_then(|(_, value)| literal_scalar(value))
                .is_some_and(|shell| matches!(shell.to_ascii_lowercase().as_str(), "pwsh" | "powershell"))
    })
}

pub(super) fn environment_variables<'a>(path: &str, source: &'a str) -> Vec<(&'a str, &'a str)> {
    if !is_github_yaml(path) {
        return Vec::new();
    }
    let lines = source.lines().collect::<Vec<_>>();
    let mut variables = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let Some((key, value)) = yaml_key_value(line) else {
            index += 1;
            continue;
        };
        if key != "env" || !value.trim().is_empty() {
            index += 1;
            continue;
        }
        let header_indentation = leading_spaces(line);
        let mut variable_indentation = None;
        index += 1;
        while index < lines.len() {
            let variable_line = lines[index];
            if variable_line.trim().is_empty() || variable_line.trim_start().starts_with('#') {
                index += 1;
                continue;
            }
            let indentation = leading_spaces(variable_line);
            if indentation <= header_indentation {
                break;
            }
            let expected_indentation = *variable_indentation.get_or_insert(indentation);
            if indentation == expected_indentation
                && let Some((name, _)) = yaml_key_value(variable_line)
            {
                variables.push((name, variable_line));
            }
            index += 1;
        }
    }
    variables
}

pub(super) fn yaml_key_value(line: &str) -> Option<(&str, &str)> {
    let mut content = line.trim_start();
    if let Some(rest) = content.strip_prefix("- ") {
        content = rest.trim_start();
    }
    let (key, value) = content.split_once(':')?;
    let key = key.trim();
    let key = key
        .strip_prefix('\'')
        .and_then(|key| key.strip_suffix('\''))
        .or_else(|| key.strip_prefix('"').and_then(|key| key.strip_suffix('"')))
        .unwrap_or(key);
    Some((key, value))
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

#[cfg(test)]
mod tests {
    use super::validate_execution_metadata;

    #[test]
    fn only_the_protected_gate_may_select_the_candidate_checkout() {
        let reviewed = "steps:\n      - name: Gate\n        working-directory: .candidate\n        run: cargo check\n";
        validate_execution_metadata(".github/workflows/trusted-maintainability.yml", reviewed).expect("reviewed candidate checkout");

        for (path, source) in [
            (".github/workflows/ci.yml", reviewed),
            (
                ".github/workflows/trusted-maintainability.yml",
                "steps:\n      - name: Gate\n        working-directory: .other\n        run: cargo check\n",
            ),
            (
                ".github/workflows/trusted-maintainability.yml",
                "steps:\n      - name: Gate\n        'working-directory': .candidate\n        run: cargo check\n",
            ),
        ] {
            assert!(validate_execution_metadata(path, source).is_err(), "{path}: {source}");
        }
    }
}
