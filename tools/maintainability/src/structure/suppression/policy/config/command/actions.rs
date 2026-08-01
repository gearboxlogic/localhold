use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::yaml::{leading_spaces, literal_scalar, yaml_key_value};

const DOWNLOAD_ACTION: &str = "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c";
const REVIEWED_REMOTE_ACTIONS: &[&str] = &[
    "actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9",
    "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0",
    "actions/dependency-review-action@a1d282b36b6f3519aa1f3fc636f609c47dddb294",
    DOWNLOAD_ACTION,
    "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
    "jdx/mise-action@e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d",
];
const REVIEWED_DOWNLOAD_DESTINATIONS: &[&str] = &["dist"];

pub(super) fn validate_local_actions(workspace: &Path, paths: &BTreeSet<String>) -> Result<()> {
    for metadata_path in paths.iter().filter(|path| is_action_metadata(path)) {
        let absolute = workspace.join(metadata_path);
        let metadata = fs::symlink_metadata(&absolute).with_context(|| format!("inspect local action metadata {metadata_path}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("local action metadata must be a regular non-symlink file: {metadata_path:?}");
        }
        let source = fs::read_to_string(&absolute).with_context(|| format!("read local action metadata {metadata_path}"))?;
        let using = runs_using(&source)?.with_context(|| format!("local action metadata {metadata_path:?} has no literal runs.using value"))?;
        if using != "composite" {
            bail!("only composite local actions are supported by command governance; {metadata_path:?} uses {using:?}");
        }
    }
    Ok(())
}

pub(super) fn validate_action_references(workspace: &Path, tracked_paths: &BTreeSet<String>, path: &str, source: &str) -> Result<()> {
    if !path.starts_with(".github/workflows") && !is_action_metadata(path) {
        return Ok(());
    }
    let lines = source.lines().collect::<Vec<_>>();
    for (line_index, line) in lines.iter().copied().enumerate() {
        let Some((key, value)) = yaml_key_value(line) else {
            continue;
        };
        if key != "uses" {
            continue;
        }
        let reference = literal_scalar(value).with_context(|| format!("GitHub action reference in {path:?} must be a literal scalar"))?;
        if let Some(relative) = audited_local_reference(&reference) {
            validate_local_reference(workspace, tracked_paths, path, relative)?;
            continue;
        }
        if REVIEWED_REMOTE_ACTIONS.contains(&reference.as_str()) {
            if reference == DOWNLOAD_ACTION {
                validate_download_destination(path, &lines, line_index)?;
            }
            continue;
        }
        bail!("GitHub action reference {reference:?} in {path:?} is not in the reviewed exact-revision allowlist");
    }
    Ok(())
}

fn validate_download_destination(path: &str, lines: &[&str], uses_index: usize) -> Result<()> {
    let uses_line = lines[uses_index];
    let sequence_indentation = leading_spaces(uses_line);
    let field_indentation = sequence_indentation + usize::from(uses_line.trim_start().starts_with("- ")) * 2;
    let mut with_indentation = None;
    let mut destinations = Vec::new();
    for line in lines.iter().copied().skip(uses_index + 1) {
        let content = line.trim_start();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        let indentation = leading_spaces(line);
        if indentation < field_indentation || indentation == sequence_indentation && content.starts_with("- ") {
            break;
        }
        if indentation == field_indentation {
            with_indentation = yaml_key_value(line).filter(|(key, value)| *key == "with" && value.trim().is_empty()).map(|_| indentation);
            continue;
        }
        if with_indentation.is_some_and(|with| indentation > with)
            && let Some(("path", value)) = yaml_key_value(line)
        {
            destinations.push(literal_scalar(value).with_context(|| format!("artifact download destination in {path:?} must be a literal scalar"))?);
        }
    }
    if destinations.len() != 1 || !REVIEWED_DOWNLOAD_DESTINATIONS.contains(&destinations[0].as_str()) {
        bail!("artifact download in {path:?} must use exactly one reviewed confined destination: {REVIEWED_DOWNLOAD_DESTINATIONS:?}");
    }
    Ok(())
}

fn audited_local_reference(reference: &str) -> Option<&str> {
    let relative = reference.strip_prefix("./")?;
    (!relative.is_empty() && Path::new(relative).components().all(|component| matches!(component, std::path::Component::Normal(_)))).then_some(relative)
}

fn validate_local_reference(workspace: &Path, tracked_paths: &BTreeSet<String>, source_path: &str, relative: &str) -> Result<()> {
    if matches!(
        Path::new(relative)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("yml" | "yaml")
    ) {
        if !relative.starts_with(".github/workflows/") {
            bail!("local reusable workflow reference {relative:?} in {source_path:?} must be under .github/workflows");
        }
        return validate_tracked_file(workspace, tracked_paths, source_path, relative);
    }

    let candidates = [format!("{relative}/action.yml"), format!("{relative}/action.yaml")];
    let matches = candidates.iter().filter(|candidate| tracked_paths.contains(candidate.as_str())).collect::<Vec<_>>();
    if matches.len() != 1 {
        bail!("local action reference {relative:?} in {source_path:?} must resolve to exactly one tracked action.yml or action.yaml");
    }
    validate_tracked_file(workspace, tracked_paths, source_path, matches[0])
}

fn validate_tracked_file(workspace: &Path, tracked_paths: &BTreeSet<String>, source_path: &str, relative: &str) -> Result<()> {
    if !tracked_paths.contains(relative) {
        bail!("local reference {relative:?} in {source_path:?} must resolve to a tracked file");
    }
    let metadata = fs::symlink_metadata(workspace.join(relative)).with_context(|| format!("inspect local reference {relative:?}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("local reference {relative:?} in {source_path:?} must resolve to a regular non-symlink file");
    }
    Ok(())
}

fn runs_using(source: &str) -> Result<Option<String>> {
    let mut runs_indentation = None;
    for line in source.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indentation = leading_spaces(line);
        if let Some(runs) = runs_indentation {
            if indentation <= runs {
                break;
            }
            let Some((key, value)) = yaml_key_value(line) else {
                continue;
            };
            if key == "using" {
                return literal_scalar(value)
                    .with_context(|| "local action execution fields must use literal scalar values")
                    .map(Some);
            }
        } else if yaml_key_value(line).is_some_and(|(key, value)| key == "runs" && value.trim().is_empty()) {
            runs_indentation = Some(indentation);
        }
    }
    Ok(None)
}

pub(super) fn is_action_metadata(path: &str) -> bool {
    matches!(
        Path::new(path).file_name().and_then(|name| name.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("action.yml" | "action.yaml")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOWNLOAD: &str = "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c";

    fn validate(source: &str) -> Result<()> {
        validate_action_references(Path::new("."), &BTreeSet::new(), ".github/workflows/test.yml", source)
    }

    #[test]
    fn artifact_downloads_require_the_reviewed_confined_destination() {
        validate(&format!("steps:\n  - uses: {DOWNLOAD}\n    with:\n      name: payload\n      path: dist\n")).expect("confined artifact download");
        for destination in [".", "./", "${{ github.workspace }}", ".github", "target"] {
            let source = format!("steps:\n  - uses: {DOWNLOAD}\n    with:\n      name: payload\n      path: {destination}\n");
            assert!(validate(&source).is_err(), "accepted {destination:?}");
        }
        assert!(validate(&format!("steps:\n  - uses: {DOWNLOAD}\n    with:\n      name: payload\n")).is_err());
    }

    #[test]
    fn artifact_download_destination_cannot_be_overridden() {
        let source = format!("steps:\n  - uses: {DOWNLOAD}\n    with:\n      path: dist\n      path: .\n");
        assert!(validate(&source).is_err());
    }
}
