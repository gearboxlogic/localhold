use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::yaml::{leading_spaces, literal_scalar, yaml_key_value};

mod cache;
mod guarded;

const DOWNLOAD_ACTION: &str = "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c";
const CHECKOUT_ACTION: &str = "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0";
const PR_CLASSIFICATION_WORKFLOW: &str = ".github/workflows/pr-classification.yml";
const PR_BASE_REVISION: &str = "${{ github.event.pull_request.base.sha }}";
const REVIEWED_REMOTE_ACTIONS: &[&str] = &[
    cache::ACTION,
    CHECKOUT_ACTION,
    guarded::DEPENDENCY_REVIEW_ACTION,
    DOWNLOAD_ACTION,
    "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
    guarded::MISE_ACTION,
];
const REVIEWED_DOWNLOAD_DESTINATIONS: &[&str] = &["dist"];

pub(super) fn validate_guarded_configuration(workspace: &Path, tracked_paths: &BTreeSet<String>) -> Result<()> {
    guarded::validate_configuration(workspace, tracked_paths)
}

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
            match reference.as_str() {
                cache::ACTION => cache::validate_inputs(path, &lines, line_index)?,
                DOWNLOAD_ACTION => validate_download_destination(path, &lines, line_index)?,
                CHECKOUT_ACTION => validate_checkout_inputs(path, &lines, line_index)?,
                guarded::DEPENDENCY_REVIEW_ACTION => guarded::validate_dependency_review_reference(path)?,
                guarded::MISE_ACTION => guarded::validate_mise_action(path, &lines, line_index)?,
                _ => {}
            }
            continue;
        }
        bail!("GitHub action reference {reference:?} in {path:?} is not in the reviewed exact-revision allowlist");
    }
    Ok(())
}

fn validate_download_destination(path: &str, lines: &[&str], uses_index: usize) -> Result<()> {
    let destinations = action_input_values(path, lines, uses_index, "path")?;
    if destinations.len() != 1 || !REVIEWED_DOWNLOAD_DESTINATIONS.contains(&destinations[0].as_str()) {
        bail!("artifact download in {path:?} must use exactly one reviewed confined destination: {REVIEWED_DOWNLOAD_DESTINATIONS:?}");
    }
    Ok(())
}

fn validate_checkout_inputs(path: &str, lines: &[&str], uses_index: usize) -> Result<()> {
    let inputs = action_inputs(path, lines, uses_index)?;
    let mut repository = None;
    let mut checkout_ref = None;
    let mut destination = None;
    let mut fetch_depth = None;
    let mut persist_credentials = None;
    for input in inputs {
        let key = input.key;
        let value = input.literal(path)?;
        let slot = match key {
            "repository" => &mut repository,
            "ref" => &mut checkout_ref,
            "path" => &mut destination,
            "fetch-depth" if matches!(value.as_str(), "0" | "1") => &mut fetch_depth,
            "persist-credentials" if value == "false" => &mut persist_credentials,
            _ => bail!("checkout input {key:?}={value:?} in {path:?} is not reviewed"),
        };
        if slot.replace(value).is_some() {
            bail!("checkout input {key:?} in {path:?} must not be repeated");
        }
    }
    let protected_gate_checkout = path == ".github/workflows/trusted-maintainability.yml"
        && matches!(
            (repository.as_deref(), checkout_ref.as_deref(), destination.as_deref()),
            (Some("${{ github.repository }}"), Some("${{ github.workflow_sha }}"), Some(".trusted-gate")) | (None, Some("${{ github.sha }}"), Some(".candidate"))
        );
    let classification_checkout = path == PR_CLASSIFICATION_WORKFLOW
        && matches!(
            (repository.as_deref(), checkout_ref.as_deref(), destination.as_deref()),
            (None, Some(PR_BASE_REVISION), None)
        )
        && fetch_depth.is_none()
        && persist_credentials.as_deref() == Some("false");
    if !protected_gate_checkout && !classification_checkout && (repository.is_some() || checkout_ref.is_some() || destination.is_some()) {
        bail!("checkout in {path:?} may select only the triggering repository and revision at the workspace root");
    }
    Ok(())
}

fn action_input_values(path: &str, lines: &[&str], uses_index: usize, selected_key: &str) -> Result<Vec<String>> {
    action_inputs(path, lines, uses_index)?
        .into_iter()
        .filter(|input| input.key == selected_key)
        .map(|input| input.literal(path))
        .collect()
}

struct ActionInput<'a> {
    key: &'a str,
    value: ActionValue<'a>,
}

enum ActionValue<'a> {
    Inline(&'a str),
    Block(Vec<&'a str>),
}

impl ActionInput<'_> {
    fn literal(&self, path: &str) -> Result<String> {
        let ActionValue::Inline(value) = &self.value else {
            bail!("action input {:?} in {path:?} must be a literal scalar", self.key);
        };
        literal_scalar(value).with_context(|| format!("action input {:?} in {path:?} must be a literal scalar", self.key))
    }

    fn lines(&self, path: &str) -> Result<Vec<String>> {
        match &self.value {
            ActionValue::Inline(value) => literal_scalar(value)
                .with_context(|| format!("action input {:?} in {path:?} must be a literal scalar", self.key))
                .map(|value| vec![value]),
            ActionValue::Block(values) if !values.is_empty() => values
                .iter()
                .map(|value| literal_scalar(value).with_context(|| format!("action input {:?} in {path:?} contains a non-literal value", self.key)))
                .collect(),
            ActionValue::Block(_) => bail!("action input {:?} in {path:?} must not be empty", self.key),
        }
    }
}

fn action_inputs<'a>(path: &str, lines: &'a [&str], uses_index: usize) -> Result<Vec<ActionInput<'a>>> {
    let uses_line = lines[uses_index];
    let sequence_indentation = leading_spaces(uses_line);
    let field_indentation = sequence_indentation + usize::from(uses_line.trim_start().starts_with("- ")) * 2;
    let mut with_indentation = None;
    let mut inputs = Vec::new();
    let mut index = uses_index + 1;
    while let Some(line) = lines.get(index).copied() {
        let content = line.trim_start();
        if content.is_empty() || content.starts_with('#') {
            index += 1;
            continue;
        }
        let indentation = leading_spaces(line);
        if indentation < field_indentation || indentation == sequence_indentation && content.starts_with("- ") {
            break;
        }
        if indentation == field_indentation {
            with_indentation = yaml_key_value(line).filter(|(key, value)| *key == "with" && value.trim().is_empty()).map(|_| indentation);
            index += 1;
            continue;
        }
        if with_indentation.is_some_and(|with| indentation > with) {
            let (key, value) = yaml_key_value(line).with_context(|| format!("action inputs in {path:?} must use simple key-value fields"))?;
            if value.trim() == "|" {
                index += 1;
                inputs.push(ActionInput {
                    key,
                    value: ActionValue::Block(block_scalar_lines(lines, &mut index, indentation)),
                });
                continue;
            }
            inputs.push(ActionInput {
                key,
                value: ActionValue::Inline(value),
            });
        }
        index += 1;
    }
    Ok(inputs)
}

fn block_scalar_lines<'a>(lines: &'a [&str], index: &mut usize, header_indentation: usize) -> Vec<&'a str> {
    let mut values = Vec::new();
    while let Some(line) = lines.get(*index).copied() {
        if line.trim().is_empty() {
            *index += 1;
            continue;
        }
        if leading_spaces(line) <= header_indentation {
            break;
        }
        values.push(line.trim());
        *index += 1;
    }
    values
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
        validate_at(".github/workflows/test.yml", source)
    }

    fn validate_at(path: &str, source: &str) -> Result<()> {
        validate_action_references(Path::new("."), &BTreeSet::new(), path, source)
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

    #[test]
    fn checkout_cannot_replace_the_candidate_workspace() {
        validate(&format!(
            "steps:\n  - uses: {CHECKOUT_ACTION}\n    with:\n      fetch-depth: 0\n      persist-credentials: false\n"
        ))
        .expect("triggering revision checkout");
        for inputs in [
            "ref: HEAD^",
            "ref: ${{ github.event.before }}",
            "path: ${{ github.workspace }}",
            "repository: attacker/example",
            "github-server-url: https://example.invalid",
        ] {
            let source = format!("steps:\n  - uses: {CHECKOUT_ACTION}\n    with:\n      {inputs}\n");
            assert!(validate(&source).is_err(), "accepted {inputs:?}");
        }
    }

    #[test]
    fn protected_workflow_checkouts_require_exact_refs_and_destinations() {
        for inputs in [
            "repository: ${{ github.repository }}\n      ref: ${{ github.workflow_sha }}\n      path: .trusted-gate\n      fetch-depth: 1\n      persist-credentials: false",
            "ref: ${{ github.sha }}\n      path: .candidate\n      fetch-depth: 0\n      persist-credentials: false",
        ] {
            let source = format!("steps:\n  - uses: {CHECKOUT_ACTION}\n    with:\n      {inputs}\n");
            validate_at(".github/workflows/trusted-maintainability.yml", &source).expect("reviewed protected checkout");
        }

        for altered in ["HEAD^", "${{ github.event.before }}", ".", "$GITHUB_WORKSPACE"] {
            let source = format!("steps:\n  - uses: {CHECKOUT_ACTION}\n    with:\n      ref: {altered}\n      path: .candidate\n");
            assert!(validate_at(".github/workflows/trusted-maintainability.yml", &source).is_err(), "accepted {altered:?}");
        }
    }

    #[test]
    fn classification_workflow_may_check_out_only_the_pull_request_base() {
        let reviewed = format!("steps:\n  - uses: {CHECKOUT_ACTION}\n    with:\n      ref: {PR_BASE_REVISION}\n      persist-credentials: false\n");
        validate_at(PR_CLASSIFICATION_WORKFLOW, &reviewed).expect("reviewed pull-request base checkout");

        for inputs in [
            format!("ref: {PR_BASE_REVISION}"),
            format!("ref: {PR_BASE_REVISION}\n      fetch-depth: 0\n      persist-credentials: false"),
            format!("ref: {PR_BASE_REVISION}\n      persist-credentials: false\n      persist-credentials: false"),
        ] {
            let source = format!("steps:\n  - uses: {CHECKOUT_ACTION}\n    with:\n      {inputs}\n");
            assert!(validate_at(PR_CLASSIFICATION_WORKFLOW, &source).is_err(), "accepted {inputs:?}");
        }

        for (path, checkout_ref) in [
            (PR_CLASSIFICATION_WORKFLOW, "${{ github.sha }}"),
            (PR_CLASSIFICATION_WORKFLOW, "${{ github.event.pull_request.head.sha }}"),
            (".github/workflows/other.yml", PR_BASE_REVISION),
        ] {
            let source = format!("steps:\n  - uses: {CHECKOUT_ACTION}\n    with:\n      ref: {checkout_ref}\n      persist-credentials: false\n");
            assert!(validate_at(path, &source).is_err(), "accepted {path:?} at {checkout_ref:?}");
        }
    }
}
