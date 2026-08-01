use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::yaml::{leading_spaces, literal_scalar, yaml_key_value};

const DOWNLOAD_ACTION: &str = "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c";
const CHECKOUT_ACTION: &str = "actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0";
const CACHE_ACTION: &str = "actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9";
const REVIEWED_REMOTE_ACTIONS: &[&str] = &[
    CACHE_ACTION,
    CHECKOUT_ACTION,
    "actions/dependency-review-action@a1d282b36b6f3519aa1f3fc636f609c47dddb294",
    DOWNLOAD_ACTION,
    "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
    "jdx/mise-action@e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d",
];
const REVIEWED_DOWNLOAD_DESTINATIONS: &[&str] = &["dist"];
const CARGO_CACHE_PATHS: &[&str] = &[".cache/localhold/cargo/registry", ".cache/localhold/cargo/git", "target"];
const CUDA_CACHE_PATH: &str = "${{ runner.temp }}/localhold-cuda-source-cache";

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
                CACHE_ACTION => validate_cache_inputs(path, &lines, line_index)?,
                DOWNLOAD_ACTION => validate_download_destination(path, &lines, line_index)?,
                CHECKOUT_ACTION => validate_checkout_inputs(path, &lines, line_index)?,
                _ => {}
            }
            continue;
        }
        bail!("GitHub action reference {reference:?} in {path:?} is not in the reviewed exact-revision allowlist");
    }
    Ok(())
}

fn validate_cache_inputs(path: &str, lines: &[&str], uses_index: usize) -> Result<()> {
    let mut cache_paths = None;
    let mut key = None;
    let mut restore_keys = None;
    for input in action_inputs(path, lines, uses_index)? {
        let slot = match input.key {
            "path" => &mut cache_paths,
            "key" => &mut key,
            "restore-keys" => &mut restore_keys,
            _ => bail!("cache input {:?} in {path:?} is not reviewed", input.key),
        };
        if slot.replace(input.lines(path)?).is_some() {
            bail!("cache input {:?} in {path:?} must not be repeated", input.key);
        }
    }
    let cache_paths = cache_paths.with_context(|| format!("cache in {path:?} must declare one reviewed path set"))?;
    let key = key.with_context(|| format!("cache in {path:?} must declare one reviewed key"))?;
    if key.len() != 1 {
        bail!("cache key in {path:?} must be one literal line");
    }
    let restore_keys = restore_keys.unwrap_or_default();
    if !reviewed_cache_profile(&cache_paths, &key[0], &restore_keys) {
        bail!("cache in {path:?} must use one exact reviewed path, key, and restore-key profile");
    }
    Ok(())
}

fn reviewed_cache_profile(paths: &[String], key: &str, restore_keys: &[String]) -> bool {
    let cargo_paths = paths.iter().map(String::as_str).eq(CARGO_CACHE_PATHS.iter().copied());
    let cargo_profile = cargo_paths
        && (key == "ubuntu-22.04-rust-${{ hashFiles('Cargo.lock', 'mise.lock') }}" && restore_keys == ["ubuntu-22.04-rust-"]
            || key == "${{ runner.os }}-rust-${{ hashFiles('Cargo.lock', 'mise.lock') }}" && restore_keys == ["${{ runner.os }}-rust-"]
            || key == "${{ runner.os }}-rust-outdated-${{ hashFiles('Cargo.lock', 'mise.lock') }}"
                && restore_keys == ["${{ runner.os }}-rust-outdated-", "${{ runner.os }}-rust-"]);
    cargo_profile || paths == [CUDA_CACHE_PATH] && key == "localhold-cuda12-${{ hashFiles('release/cuda-linux-x86_64.json') }}" && restore_keys.is_empty()
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
    for input in inputs {
        let key = input.key;
        let value = input.literal(path)?;
        let slot = match key {
            "repository" => &mut repository,
            "ref" => &mut checkout_ref,
            "path" => &mut destination,
            "fetch-depth" if matches!(value.as_str(), "0" | "1") => continue,
            "persist-credentials" if value == "false" => continue,
            _ => bail!("checkout input {key:?}={value:?} in {path:?} is not reviewed"),
        };
        if slot.replace(value).is_some() {
            bail!("checkout input {key:?} in {path:?} must not be repeated");
        }
    }
    let trusted_checkout = path == ".github/workflows/trusted-maintainability.yml"
        && matches!(
            (repository.as_deref(), checkout_ref.as_deref(), destination.as_deref()),
            (Some("${{ github.repository }}"), Some("${{ github.workflow_sha }}"), Some(".trusted-gate")) | (None, Some("${{ github.sha }}"), Some(".candidate"))
        );
    if !trusted_checkout && (repository.is_some() || checkout_ref.is_some() || destination.is_some()) {
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
    fn caches_require_exact_confined_profiles() {
        let cargo = "path: |\n        .cache/localhold/cargo/registry\n        .cache/localhold/cargo/git\n        target\n      key: ${{ runner.os }}-rust-${{ hashFiles('Cargo.lock', 'mise.lock') }}\n      restore-keys: ${{ runner.os }}-rust-";
        let outdated = "path: |\n        .cache/localhold/cargo/registry\n        .cache/localhold/cargo/git\n        target\n      key: ${{ runner.os }}-rust-outdated-${{ hashFiles('Cargo.lock', 'mise.lock') }}\n      restore-keys: |\n        ${{ runner.os }}-rust-outdated-\n        ${{ runner.os }}-rust-";
        let cuda = "path: ${{ runner.temp }}/localhold-cuda-source-cache\n      key: localhold-cuda12-${{ hashFiles('release/cuda-linux-x86_64.json') }}";
        for inputs in [cargo, outdated, cuda] {
            validate(&format!("steps:\n  - uses: {CACHE_ACTION}\n    with:\n      {inputs}\n")).expect("reviewed cache profile");
        }

        for inputs in [
            cargo.replace("        target", "        Justfile"),
            cargo.replace("${{ runner.os }}-rust-${{ hashFiles('Cargo.lock', 'mise.lock') }}", "attacker-controlled"),
            cargo.replace("path: |", "path: Justfile\n      path: |"),
            cuda.replace("${{ runner.temp }}/localhold-cuda-source-cache", "${{ github.workspace }}"),
        ] {
            assert!(
                validate(&format!("steps:\n  - uses: {CACHE_ACTION}\n    with:\n      {inputs}\n")).is_err(),
                "accepted {inputs:?}"
            );
        }
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
}
