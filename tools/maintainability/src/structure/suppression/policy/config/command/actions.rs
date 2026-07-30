use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::yaml::{leading_spaces, literal_scalar, yaml_key_value};

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

fn is_action_metadata(path: &str) -> bool {
    matches!(
        Path::new(path).file_name().and_then(|name| name.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("action.yml" | "action.yaml")
    )
}
