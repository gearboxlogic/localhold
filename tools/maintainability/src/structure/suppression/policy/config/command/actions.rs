use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::yaml::{leading_spaces, literal_scalar, yaml_key_value};

const REVIEWED_REMOTE_ACTIONS: &[&str] = &[
    "actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9",
    "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0",
    "actions/dependency-review-action@a1d282b36b6f3519aa1f3fc636f609c47dddb294",
    "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c",
    "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
    "jdx/mise-action@e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d",
];

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
    for line in source.lines() {
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
            continue;
        }
        bail!("GitHub action reference {reference:?} in {path:?} is not in the reviewed exact-revision allowlist");
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
