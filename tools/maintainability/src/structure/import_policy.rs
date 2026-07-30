use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::classify::Inventory;
use super::revision::maintainability_base_revision;

const CURRENT_SCHEMA_VERSION: u32 = 1;
const POLICY_PATH: &str = "policy/maintainability/architecture.json";
const HTTP_TRANSPORT_PATH: &str = "src/http_transport.rs";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ImportPolicy {
    schema_version: u32,
    baseline_commit: String,
    exceptions: Vec<ImportException>,
    retirements: Vec<ImportRetirement>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ImportException {
    id: String,
    source: String,
    target: String,
    baseline: bool,
    owner: String,
    issue: String,
    pull_request: String,
    rationale: String,
    re_review_phase: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ImportRetirement {
    id: String,
    exception_id: String,
    owner: String,
    issue: String,
    pull_request: String,
    rationale: String,
}

impl ImportPolicy {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read architecture policy {}", path.display()))?;
        let policy: Self = serde_json::from_slice(&bytes).with_context(|| format!("parse architecture policy {}", path.display()))?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn compare_current(&self, inventory: &Inventory) -> Result<()> {
        let retired = self.retired_exception_ids();
        compare_inventory("current", inventory, self.exceptions.iter().filter(|exception| !retired.contains(exception.id.as_str())))
    }

    pub fn compare_baseline(&self, inventory: &Inventory) -> Result<()> {
        compare_inventory("baseline", inventory, self.exceptions.iter().filter(|exception| exception.baseline))
    }

    pub fn require_baseline_commit(&self, expected: &str) -> Result<()> {
        if self.baseline_commit != expected {
            bail!("architecture and structure policies must use the same baseline commit");
        }
        Ok(())
    }

    pub fn compare_previous_revision(&self, workspace: &Path) -> Result<()> {
        let Some(revision) = maintainability_base_revision()? else {
            return Ok(());
        };
        validate_revision(&revision)?;
        let object = format!("{revision}:{POLICY_PATH}");
        let output = crate::structure::revision::git_command()
            .current_dir(workspace)
            .args(["show", "--no-ext-diff", &object])
            .output()
            .context("read architecture policy from maintainability base revision")?;
        if !output.status.success() {
            verify_initial_policy_revision(workspace, &revision, &object)?;
            return self.validate_initial_policy();
        }
        let previous: Self = serde_json::from_slice(&output.stdout).context("parse architecture policy from maintainability base revision")?;
        previous.validate()?;
        self.compare_policy(&previous)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            bail!("unsupported architecture policy schema {}", self.schema_version);
        }
        validate_revision(&self.baseline_commit)?;
        let mut ids = BTreeSet::new();
        let mut imports = BTreeSet::new();
        for exception in &self.exceptions {
            validate_id(&exception.id)?;
            if !ids.insert(exception.id.as_str()) {
                bail!("duplicate architecture import exception ID {:?}", exception.id);
            }
            validate_relative_rust_path(&exception.source)?;
            if exception.source != HTTP_TRANSPORT_PATH {
                bail!("architecture import exception {:?} may apply only to {HTTP_TRANSPORT_PATH}", exception.id);
            }
            validate_target(&exception.target)?;
            if !imports.insert((exception.source.as_str(), exception.target.as_str())) {
                bail!("duplicate architecture import exception for {:?} importing {:?}", exception.source, exception.target);
            }
            require_text(&exception.id, "owner", &exception.owner)?;
            require_text(&exception.id, "issue", &exception.issue)?;
            require_text(&exception.id, "pull request", &exception.pull_request)?;
            require_text(&exception.id, "rationale", &exception.rationale)?;
            require_text(&exception.id, "re-review phase", &exception.re_review_phase)?;
        }
        let exception_ids = ids.clone();
        let mut retired = BTreeSet::new();
        for retirement in &self.retirements {
            validate_id(&retirement.id)?;
            if !ids.insert(retirement.id.as_str()) {
                bail!("duplicate architecture policy evidence ID {:?}", retirement.id);
            }
            if !exception_ids.contains(retirement.exception_id.as_str()) {
                bail!(
                    "architecture import retirement {:?} references unknown exception {:?}",
                    retirement.id,
                    retirement.exception_id
                );
            }
            if !retired.insert(retirement.exception_id.as_str()) {
                bail!("architecture import exception {:?} is retired more than once", retirement.exception_id);
            }
            require_text(&retirement.id, "owner", &retirement.owner)?;
            require_text(&retirement.id, "issue", &retirement.issue)?;
            require_text(&retirement.id, "pull request", &retirement.pull_request)?;
            require_text(&retirement.id, "rationale", &retirement.rationale)?;
        }
        Ok(())
    }

    fn compare_policy(&self, previous: &Self) -> Result<()> {
        if self.baseline_commit != previous.baseline_commit {
            bail!("architecture import baseline commit is immutable");
        }
        if self.exceptions.len() < previous.exceptions.len() || self.exceptions.get(..previous.exceptions.len()) != Some(&previous.exceptions) {
            bail!("architecture import exception ledger is append-only and existing evidence is immutable");
        }
        if self.exceptions[previous.exceptions.len()..].iter().any(|exception| exception.baseline) {
            bail!("new architecture import exceptions cannot be marked as baseline evidence");
        }
        if self.retirements.len() < previous.retirements.len() || self.retirements.get(..previous.retirements.len()) != Some(&previous.retirements) {
            bail!("architecture import retirement ledger is append-only and existing evidence is immutable");
        }
        let previous_exception_ids = previous.exceptions.iter().map(|exception| exception.id.as_str()).collect::<BTreeSet<_>>();
        if self.retirements[previous.retirements.len()..]
            .iter()
            .any(|retirement| !previous_exception_ids.contains(retirement.exception_id.as_str()))
        {
            bail!("an architecture import exception cannot be added and retired in the same policy revision");
        }
        Ok(())
    }

    fn validate_initial_policy(&self) -> Result<()> {
        if self.exceptions.iter().any(|exception| !exception.baseline) || !self.retirements.is_empty() {
            bail!("the initial architecture policy may contain only active recovery-baseline evidence");
        }
        Ok(())
    }

    fn retired_exception_ids(&self) -> BTreeSet<&str> {
        self.retirements.iter().map(|retirement| retirement.exception_id.as_str()).collect()
    }
}

fn compare_inventory<'a>(label: &str, inventory: &Inventory, exceptions: impl Iterator<Item = &'a ImportException>) -> Result<()> {
    let mut observed = inventory
        .files
        .iter()
        .flat_map(|file| file.production_internal_imports.iter().map(|target| (file.path.as_str(), target.as_str())))
        .collect::<Vec<_>>();
    observed.sort_unstable();
    let mut expected = exceptions.map(|exception| (exception.source.as_str(), exception.target.as_str())).collect::<Vec<_>>();
    expected.sort_unstable();
    if observed != expected {
        bail!("architecture {label} restricted import mismatch: expected={expected:?}, observed={observed:?}");
    }
    Ok(())
}

fn validate_target(target: &str) -> Result<()> {
    let Some(remainder) = target.strip_prefix("crate::server::") else {
        bail!("architecture import exception target must name one explicit crate::server item");
    };
    if remainder.is_empty() || remainder.split("::").any(|segment| !is_normalized_identifier(segment)) {
        bail!("architecture import exception target must be a normalized explicit crate::server path");
    }
    Ok(())
}

fn is_normalized_identifier(segment: &str) -> bool {
    let mut bytes = segment.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_') && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn validate_id(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_'))
    {
        bail!("architecture import exception ID must use lowercase ASCII letters, digits, '.', '-', or '_'");
    }
    Ok(())
}

fn validate_relative_rust_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    if path.is_absolute()
        || path.extension().and_then(|extension| extension.to_str()) != Some("rs")
        || path.components().any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("architecture policy path must be a normalized relative Rust path: {value:?}");
    }
    Ok(())
}

fn validate_revision(revision: &str) -> Result<()> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("architecture baseline revision must be a full Git commit hash");
    }
    Ok(())
}

fn require_text(id: &str, label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("architecture import exception {id:?} {label} must not be empty");
    }
    Ok(())
}

fn verify_initial_policy_revision(workspace: &Path, revision: &str, object: &str) -> Result<()> {
    let status = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("verify architecture policy base revision")?;
    if !status.success() {
        bail!("architecture policy base revision {revision} is not available");
    }
    let status = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["cat-file", "-e", object])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("inspect architecture policy in base revision")?;
    if status.success() {
        bail!("architecture policy exists in base revision but could not be read");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
