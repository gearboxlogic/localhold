use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;

use self::config::{compare_cargo_allows, compare_clippy_configuration, reject_checked_in_weakening, validate_cargo_allowances, validate_clippy_configuration};
use self::model::{CargoAllowanceFile, ClippyConfigurationFile, Policy};
use self::source::{SourceCounts, compare_current, load_baselines, validate_exceptions, validate_governance};
use super::SourceSuppression;

mod config;
mod model;
mod revision;
mod source;
#[cfg(test)]
mod tests;

const CURRENT_SCHEMA_VERSION: u32 = 1;
pub(super) const POLICY_PATH: &str = "policy/maintainability/lint-suppressions.json";
const POLICY_ROOT: &str = "policy/maintainability/lint-suppressions";

pub(in crate::structure) struct SuppressionPolicy {
    model: Policy,
    source_baseline: SourceCounts,
    cargo_allowances: CargoAllowanceFile,
    clippy_configuration: ClippyConfigurationFile,
}

impl SuppressionPolicy {
    pub(in crate::structure) fn load(workspace: &Path) -> Result<Self> {
        let model: Policy = load_policy_file(workspace, POLICY_PATH)?;
        Self::from_model(workspace, model)
    }

    fn from_model(workspace: &Path, model: Policy) -> Result<Self> {
        validate_model(&model)?;
        let source_baseline = load_baselines(workspace, &model.source_baselines)?;
        let cargo_allowances = load_cargo_allowance_files(&model, |path| load_policy_file(workspace, path))?;
        let clippy_configuration: ClippyConfigurationFile = load_policy_file(workspace, &model.clippy_configuration_path)?;
        Self::assemble(model, source_baseline, cargo_allowances, clippy_configuration)
    }

    fn assemble(model: Policy, source_baseline: SourceCounts, cargo_allowances: CargoAllowanceFile, clippy_configuration: ClippyConfigurationFile) -> Result<Self> {
        validate_model(&model)?;
        if cargo_allowances.schema_version != CURRENT_SCHEMA_VERSION {
            bail!("unsupported Cargo allowance policy schema {}", cargo_allowances.schema_version);
        }
        validate_cargo_allowances(&cargo_allowances.entries)?;
        validate_clippy_configuration(&clippy_configuration)?;
        Ok(Self {
            model,
            source_baseline,
            cargo_allowances,
            clippy_configuration,
        })
    }

    pub(in crate::structure) fn compare_current(&self, workspace: &Path, sites: &[SourceSuppression]) -> Result<SourceCounts> {
        super::reject_tooling_suppressions(workspace)?;
        let observed = compare_current(sites, &self.source_baseline, &self.model.source_exceptions, false)?;
        compare_cargo_allows(workspace, &self.cargo_allowances.entries)?;
        compare_clippy_configuration(workspace, &self.clippy_configuration.entries)?;
        let direct_sources = reject_checked_in_weakening(workspace)?;
        super::reject_direct_source_suppressions(workspace, &direct_sources)?;
        Ok(observed)
    }
}

fn load_cargo_allowance_files(model: &Policy, mut load: impl FnMut(&str) -> Result<CargoAllowanceFile>) -> Result<CargoAllowanceFile> {
    if model.cargo_allowances_paths.is_empty() {
        bail!("lint-suppression policy must reference at least one Cargo allowance fragment");
    }
    let mut entries = Vec::new();
    for path in &model.cargo_allowances_paths {
        let fragment = load(path)?;
        if fragment.schema_version != CURRENT_SCHEMA_VERSION {
            bail!("unsupported Cargo allowance policy schema {}", fragment.schema_version);
        }
        entries.extend(fragment.entries);
    }
    Ok(CargoAllowanceFile {
        schema_version: CURRENT_SCHEMA_VERSION,
        entries,
    })
}

fn validate_model(model: &Policy) -> Result<()> {
    if model.schema_version != CURRENT_SCHEMA_VERSION {
        bail!("unsupported lint-suppression policy schema {}", model.schema_version);
    }
    validate_revision(&model.adoption_commit)?;
    validate_governance(&model.source_governance)?;
    validate_exceptions(&model.source_exceptions)
}

pub(super) fn load_policy_file<T: DeserializeOwned>(workspace: &Path, relative: &str) -> Result<T> {
    let path = checked_policy_path(workspace, relative)?;
    let worktree_bytes = fs::read(&path).with_context(|| format!("read lint-suppression policy {}", path.display()))?;
    let object = format!("HEAD:{relative}");
    let reviewed = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["show", "--no-ext-diff", &object])
        .output()
        .with_context(|| format!("read lint-suppression policy {relative:?} from checked-out revision"))?;
    if !reviewed.status.success() {
        bail!("lint-suppression policy {relative:?} is unreadable in the checked-out revision");
    }
    if worktree_bytes != reviewed.stdout {
        bail!("lint-suppression policy {relative:?} differs from the checked-out revision");
    }
    serde_json::from_slice(&reviewed.stdout).with_context(|| format!("parse lint-suppression policy {relative:?} from checked-out revision"))
}

fn checked_policy_path(workspace: &Path, relative: &str) -> Result<PathBuf> {
    validate_policy_relative(relative)?;
    let relative_path = Path::new(relative);
    reject_symlinked_policy_components(workspace, relative_path)?;
    let allowed_root = if relative == POLICY_PATH { "policy/maintainability" } else { POLICY_ROOT };
    let policy_root = fs::canonicalize(workspace.join(allowed_root)).with_context(|| format!("resolve lint-suppression policy root {allowed_root}"))?;
    let path = workspace.join(relative_path);
    let metadata = fs::symlink_metadata(&path).with_context(|| format!("inspect lint-suppression policy {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("lint-suppression policy must be a regular non-symlink file: {relative:?}");
    }
    let canonical = fs::canonicalize(&path).with_context(|| format!("resolve lint-suppression policy {}", path.display()))?;
    if !canonical.starts_with(&policy_root) {
        bail!("lint-suppression policy escapes its policy root: {relative:?}");
    }
    Ok(path)
}

fn reject_symlinked_policy_components(workspace: &Path, relative: &Path) -> Result<()> {
    let mut candidate = workspace.to_path_buf();
    for component in relative.components() {
        candidate.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&candidate).with_context(|| format!("inspect lint-suppression policy path component {}", candidate.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("lint-suppression policy path components must not be symlinks: {}", candidate.display());
        }
    }
    Ok(())
}

fn validate_policy_relative(relative: &str) -> Result<()> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path.components().any(|component| !matches!(component, Component::Normal(_)))
        || relative != POLICY_PATH && !relative.starts_with(&format!("{POLICY_ROOT}/"))
        || relative_path.extension().and_then(|extension| extension.to_str()) != Some("json")
    {
        bail!("lint-suppression policy path must remain under {POLICY_ROOT}: {relative:?}");
    }
    Ok(())
}

pub(super) fn require_id(kind: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_'))
    {
        bail!("{kind} ID must use lowercase ASCII letters, digits, '.', '-', or '_'");
    }
    Ok(())
}

pub(super) fn require_text(id: &str, label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("lint-suppression policy {id:?} {label} must not be empty");
    }
    Ok(())
}

fn validate_revision(revision: &str) -> Result<()> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("lint-suppression adoption revision must be a full Git commit hash");
    }
    Ok(())
}
