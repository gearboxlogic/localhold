use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::workspace_relative_path;

pub(super) fn collect(message: &Value, dep_info: &mut BTreeSet<PathBuf>) -> Result<()> {
    let filenames = message.get("filenames").and_then(Value::as_array).context("compiler artifact is missing filenames")?;
    let mut found = false;
    for filename in filenames {
        let artifact = PathBuf::from(filename.as_str().context("compiler artifact filename is not a string")?);
        let Some(stem) = artifact.file_stem().and_then(OsStr::to_str) else {
            continue;
        };
        let stem = stem.strip_prefix("lib").unwrap_or(stem);
        let candidate = artifact.with_file_name(format!("{stem}.d"));
        if candidate.try_exists().with_context(|| format!("inspect dep-info candidate {}", candidate.display()))? {
            dep_info.insert(candidate);
            found = true;
        }
    }
    if !found {
        let target = message.pointer("/target/name").and_then(Value::as_str).unwrap_or("<unknown>");
        bail!("compiler artifact for root target {target} has no readable dep-info");
    }
    Ok(())
}

pub(super) fn verify(workspace: &Path, dep_info: &BTreeSet<PathBuf>) -> Result<()> {
    let mut inputs = BTreeSet::new();
    for path in dep_info {
        let source = fs::read_to_string(path).with_context(|| format!("read dep-info {}", path.display()))?;
        inputs.extend(parse(&source).with_context(|| format!("parse dep-info {}", path.display()))?);
    }
    for input in inputs {
        let relative = workspace_relative_path(workspace, Path::new(&input))?;
        if !is_audited_compiler_input(Path::new(&relative)) {
            bail!("compiler-expanded source graph includes unaudited input {relative}");
        }
    }
    Ok(())
}

pub(super) fn parse(source: &str) -> Result<Vec<String>> {
    let logical = source.replace("\\\r\n", "").replace("\\\n", "");
    let mut dependencies = Vec::new();
    for rule in logical.lines().filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#')) {
        let Some((_, values)) = rule.split_once(": ") else {
            if rule.ends_with(':') {
                continue;
            }
            bail!("dep-info rule is missing ': ' separator: {rule:?}");
        };
        dependencies.extend(parse_make_words(values));
    }
    Ok(dependencies)
}

pub(super) fn parse_make_words(source: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut escaped = false;
    for character in source.chars() {
        if escaped {
            if !character.is_whitespace() && !matches!(character, '#' | ':' | '\\') {
                word.push('\\');
            }
            word.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character.is_whitespace() {
            if !word.is_empty() {
                words.push(restore_windows_verbatim_prefix(std::mem::take(&mut word)));
            }
        } else {
            word.push(character);
        }
    }
    if escaped {
        word.push('\\');
    }
    if !word.is_empty() {
        words.push(restore_windows_verbatim_prefix(word));
    }
    words
}

fn restore_windows_verbatim_prefix(mut word: String) -> String {
    if word.starts_with(r"\?\") {
        word.insert(0, '\\');
    }
    word
}

pub(super) fn is_audited_compiler_input(path: &Path) -> bool {
    if path == Path::new("Cargo.toml") || path == Path::new("clippy.toml") {
        return true;
    }
    path.components().next().is_some_and(|component| {
        let value = component.as_os_str();
        value == "src" || value == "tests" || value == "benches" || value == "examples"
    })
}
