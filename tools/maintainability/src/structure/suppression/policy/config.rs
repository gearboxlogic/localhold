use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::model::{CargoAllowance, ClippyConfigurationFile, ClippyConstraint, ClippySetting, Disposition, Status};
use super::{require_id, require_text};

mod cargo;
mod command;
#[cfg(test)]
mod tests;

use cargo::{scan_cargo_allows, tracked_manifests};
pub(super) use command::reject_checked_in_weakening;
#[cfg(test)]
use command::{
    BOOTSTRAP_ENVIRONMENT_LINES, BOOTSTRAP_TEST_ENVIRONMENT_LINES, CI_TRUST_ENVIRONMENT_LINES, GATE_RUNNER_COMMAND_LINES, GATE_RUNNER_ENVIRONMENT_LINES,
    GPU_RELEASE_REVISION_ENVIRONMENT_LINES, MISE_ENVIRONMENT_LINES, RUNNER_COMMAND_LINES, RUNNER_ENVIRONMENT_LINES, TRUSTED_GATE_ENVIRONMENT_LINES, has_sourced_file_indirection,
    is_execution_surface, scrubber_environment_references_are_exact, weakening_environment, weakening_environment_for_surface, weakening_token, weakening_token_for_surface,
};

pub(super) fn validate_cargo_allowances(entries: &[CargoAllowance]) -> Result<()> {
    let mut ids = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for entry in entries {
        require_id("Cargo allowance", &entry.id)?;
        if !ids.insert(entry.id.as_str()) {
            bail!("duplicate Cargo lint allowance ID {:?}", entry.id);
        }
        validate_relative_path(&entry.manifest, "Cargo allowance manifest")?;
        require_name("Cargo lint family", &entry.family)?;
        require_name("Cargo lint", &entry.lint)?;
        if !keys.insert((entry.manifest.as_str(), entry.family.as_str(), entry.lint.as_str())) {
            bail!("duplicate Cargo lint allowance for {} {}::{}", entry.manifest, entry.family, entry.lint);
        }
        for (label, value) in [
            ("owner", entry.owner.as_str()),
            ("issue", entry.issue.as_str()),
            ("pull request", entry.pull_request.as_str()),
            ("rationale", entry.rationale.as_str()),
            ("safety invariant", entry.safety_invariant.as_str()),
            ("alternatives considered", entry.alternatives_considered.as_str()),
            ("substitute", entry.substitute.as_str()),
            ("sentinel", entry.sentinel.as_str()),
            ("evidence", entry.evidence.as_str()),
            ("re-review phase", entry.re_review_phase.as_str()),
        ] {
            require_text(&entry.id, label, value)?;
        }
        validate_temporary_fields(&entry.id, entry.disposition, entry.removal_issue.as_deref(), entry.removal_phase.as_deref())?;
    }
    Ok(())
}

pub(super) fn compare_cargo_allows(workspace: &Path, entries: &[CargoAllowance]) -> Result<()> {
    let observed = scan_cargo_allows(workspace)?;
    let expected = entries
        .iter()
        .filter(|entry| entry.status == Status::Active)
        .map(|entry| (entry.manifest.clone(), entry.family.clone(), entry.lint.clone(), entry.priority))
        .collect::<BTreeSet<_>>();
    if observed != expected {
        bail!("Cargo lint allow configuration differs from reviewed policy: expected={expected:?}, observed={observed:?}");
    }
    Ok(())
}

pub(super) fn validate_clippy_configuration(policy: &ClippyConfigurationFile) -> Result<()> {
    if policy.schema_version != 1 {
        bail!("unsupported Clippy configuration policy schema {}", policy.schema_version);
    }
    let mut ids = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for entry in &policy.entries {
        require_id("Clippy configuration", &entry.id)?;
        require_name("Clippy configuration key", &entry.key)?;
        if !ids.insert(entry.id.as_str()) || !keys.insert(entry.key.as_str()) {
            bail!("duplicate Clippy configuration policy ID or key");
        }
        for (label, value) in [
            ("owner", entry.owner.as_str()),
            ("issue", entry.issue.as_str()),
            ("pull request", entry.pull_request.as_str()),
            ("rationale", entry.rationale.as_str()),
            ("safety invariant", entry.safety_invariant.as_str()),
            ("alternatives considered", entry.alternatives_considered.as_str()),
            ("sentinel", entry.sentinel.as_str()),
            ("evidence", entry.evidence.as_str()),
            ("re-review phase", entry.re_review_phase.as_str()),
        ] {
            require_text(&entry.id, label, value)?;
        }
        validate_clippy_constraint(entry)?;
    }
    Ok(())
}

pub(super) fn compare_clippy_configuration(workspace: &Path, entries: &[ClippySetting]) -> Result<()> {
    let path = workspace.join("clippy.toml");
    let metadata = fs::symlink_metadata(&path).with_context(|| format!("inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("clippy.toml must be a regular non-symlink file");
    }
    reject_alternate_clippy_configuration(workspace)?;
    let source = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let parsed = source.parse::<toml::Table>().context("parse clippy.toml")?;
    let active = entries.iter().map(|entry| (entry.key.as_str(), entry)).collect::<BTreeMap<_, _>>();
    let observed = parsed.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = active.keys().copied().collect::<BTreeSet<_>>();
    if observed != expected {
        bail!("clippy.toml keys differ from reviewed policy: expected={expected:?}, observed={observed:?}");
    }
    for (key, setting) in active {
        compare_clippy_value(key, &parsed[key], &setting.constraint)?;
    }
    Ok(())
}

pub(super) fn compare_clippy_previous_revision(workspace: &Path, revision: &str, previous_entries: &[ClippySetting]) -> Result<()> {
    let current_source = fs::read_to_string(workspace.join("clippy.toml")).context("read current clippy.toml")?;
    let current = current_source.parse::<toml::Table>().context("parse current clippy.toml")?;
    let object = format!("{revision}:clippy.toml");
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["show", "--no-ext-diff", &object])
        .output()
        .context("read clippy.toml from maintainability base revision")?;
    if !output.status.success() {
        bail!("maintainability base revision has no readable clippy.toml");
    }
    let previous_source = String::from_utf8(output.stdout).context("previous clippy.toml is not UTF-8")?;
    let previous = previous_source.parse::<toml::Table>().context("parse previous clippy.toml")?;
    for entry in previous_entries {
        let current_value = current
            .get(&entry.key)
            .with_context(|| format!("current clippy.toml is missing reviewed key {:?}", entry.key))?;
        let previous_value = previous
            .get(&entry.key)
            .with_context(|| format!("previous clippy.toml is missing reviewed key {:?}", entry.key))?;
        compare_clippy_ratchet(&entry.key, current_value, previous_value, &entry.constraint)?;
    }
    Ok(())
}

pub(super) fn parse_nul_paths(output: &[u8], include: impl Fn(&str) -> bool) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for raw in output.split(|byte| *byte == b'\0').filter(|path| !path.is_empty()) {
        let path = std::str::from_utf8(raw).context("tracked lint-policy path is not UTF-8")?;
        validate_relative_path(path, "tracked lint-policy path")?;
        if include(path) {
            paths.push(path.to_owned());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn compare_clippy_value(key: &str, actual: &toml::Value, constraint: &ClippyConstraint) -> Result<()> {
    match constraint {
        ClippyConstraint::MaximumInteger { value } => {
            let actual = actual.as_integer().with_context(|| format!("Clippy setting {key:?} must be an integer"))?;
            if actual <= 0 || actual > *value {
                bail!("Clippy setting {key:?} must remain positive and no greater than its reviewed maximum {value}");
            }
        }
        ClippyConstraint::StringSubset { values } => {
            let actual = actual.as_array().with_context(|| format!("Clippy setting {key:?} must be an array"))?;
            let actual = actual
                .iter()
                .map(|value| value.as_str().with_context(|| format!("Clippy setting {key:?} contains a non-string value")))
                .collect::<Result<BTreeSet<_>>>()?;
            let allowed = values.iter().map(String::as_str).collect::<BTreeSet<_>>();
            if !actual.is_subset(&allowed) {
                bail!("Clippy setting {key:?} expands its reviewed allowlist");
            }
        }
    }
    Ok(())
}

fn compare_clippy_ratchet(key: &str, current: &toml::Value, previous: &toml::Value, constraint: &ClippyConstraint) -> Result<()> {
    match constraint {
        ClippyConstraint::MaximumInteger { .. } => {
            let current = current.as_integer().with_context(|| format!("current Clippy setting {key:?} must be an integer"))?;
            let previous = previous.as_integer().with_context(|| format!("previous Clippy setting {key:?} must be an integer"))?;
            if current > previous {
                bail!("Clippy threshold {key:?} cannot rise from its previous-revision value");
            }
        }
        ClippyConstraint::StringSubset { .. } => {
            let current = string_set(key, current, "current")?;
            let previous = string_set(key, previous, "previous")?;
            if !current.is_subset(&previous) {
                bail!("Clippy allowlist {key:?} cannot expand beyond its previous-revision value");
            }
        }
    }
    Ok(())
}

fn string_set<'a>(key: &str, value: &'a toml::Value, revision: &str) -> Result<BTreeSet<&'a str>> {
    value
        .as_array()
        .with_context(|| format!("{revision} Clippy setting {key:?} must be an array"))?
        .iter()
        .map(|value| value.as_str().with_context(|| format!("{revision} Clippy setting {key:?} contains a non-string value")))
        .collect()
}

fn validate_clippy_constraint(entry: &ClippySetting) -> Result<()> {
    match &entry.constraint {
        ClippyConstraint::MaximumInteger { value } if *value <= 0 => {
            bail!("Clippy configuration {:?} maximum must be positive", entry.id);
        }
        ClippyConstraint::StringSubset { values } if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) => {
            bail!("Clippy configuration {:?} string allowlist must contain non-empty values", entry.id);
        }
        ClippyConstraint::StringSubset { values } if values.iter().collect::<BTreeSet<_>>().len() != values.len() => {
            bail!("Clippy configuration {:?} string allowlist contains duplicates", entry.id);
        }
        ClippyConstraint::MaximumInteger { .. } | ClippyConstraint::StringSubset { .. } => {}
    }
    Ok(())
}

fn validate_temporary_fields(id: &str, disposition: Disposition, removal_issue: Option<&str>, removal_phase: Option<&str>) -> Result<()> {
    match disposition {
        Disposition::Permanent if removal_issue.is_some() || removal_phase.is_some() => {
            bail!("permanent Cargo lint allowance {id:?} cannot carry temporary removal fields");
        }
        Disposition::Temporary => {
            require_text(id, "removal issue", removal_issue.unwrap_or_default())?;
            require_text(id, "removal phase", removal_phase.unwrap_or_default())?;
        }
        Disposition::Permanent => {}
    }
    Ok(())
}

fn require_name(label: &str, value: &str) -> Result<()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')) {
        bail!("{label} must use lowercase ASCII letters, digits, '-', or '_'");
    }
    Ok(())
}

fn validate_relative_path(value: &str, label: &str) -> Result<()> {
    let path = Path::new(value);
    if path.is_absolute() || path.components().any(|component| !matches!(component, Component::Normal(_))) {
        bail!("{label} must be a normalized relative path: {value:?}");
    }
    Ok(())
}

fn reject_alternate_clippy_configuration(workspace: &Path) -> Result<()> {
    let mut directories = BTreeSet::from([PathBuf::new()]);
    for manifest in tracked_manifests(workspace)? {
        let parent = Path::new(&manifest).parent().context("Cargo manifest has no parent directory")?;
        directories.extend(parent.ancestors().map(Path::to_path_buf));
    }
    for directory in directories {
        for name in ["clippy.toml", ".clippy.toml"] {
            let relative = directory.join(name);
            if relative == Path::new("clippy.toml") {
                continue;
            }
            let candidate = workspace.join(&relative);
            match fs::symlink_metadata(&candidate) {
                Ok(_) => {
                    let relative = relative.display();
                    bail!("alternate Clippy configuration is unsupported; remove {relative} and govern the root clippy.toml");
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error).with_context(|| format!("inspect alternate Clippy configuration {}", candidate.display())),
            }
        }
    }
    Ok(())
}
