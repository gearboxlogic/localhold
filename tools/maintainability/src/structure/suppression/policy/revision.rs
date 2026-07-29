use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;

use super::config::compare_clippy_previous_revision;
use super::model::{CargoAllowance, CargoAllowanceFile, ClippyConfigurationFile, ClippySetting, Policy, SourceBaseline, SourceException, Status};
use super::source::{added_exception_capacity, collect_baselines, compare_current, require_count_subset};
use super::{POLICY_PATH, SuppressionPolicy, validate_model, validate_policy_relative, validate_revision};
use crate::structure::manifest::PreviousRevision;
use crate::structure::revision::maintainability_base_revision;
use crate::structure::suppression::{self, SourceSuppression};

impl SuppressionPolicy {
    pub(in crate::structure) fn compare_previous_revision(
        &self,
        workspace: &Path,
        current_sites: &[SourceSuppression],
        current_counts: &super::source::SourceCounts,
        previous_structure: Option<&PreviousRevision>,
    ) -> Result<()> {
        let Some(revision) = maintainability_base_revision()? else {
            return Ok(());
        };
        validate_revision(&revision)?;
        let Some(previous) = load_revision(workspace, &revision)? else {
            if self.model.adoption_commit != revision {
                bail!("initial lint-suppression policy adoption must name the pull-request base commit");
            }
            compare_current(current_sites, &self.source_baseline, &self.model.source_exceptions, true)?;
            return Ok(());
        };
        self.compare_policy(&previous)?;
        let previous_structure = previous_structure.context("structure policy is unavailable for lint-suppression previous-revision comparison")?;
        let component_paths = previous_structure.manifest.current_component_paths()?;
        let previous_sites = suppression::scan_revision(workspace, &revision, &previous_structure.inventory, &component_paths)?;
        let previous_counts = compare_current(&previous_sites, &previous.source_baseline, &previous.model.source_exceptions, false)?;
        let mut allowed = previous_counts;
        for (site, count) in added_exception_capacity(&self.model.source_exceptions, &previous.model.source_exceptions)? {
            let allowed_count = allowed.entry(site).or_insert(0_usize);
            *allowed_count = allowed_count.checked_add(count).context("lint-suppression previous-revision allowance overflow")?;
        }
        require_count_subset("previous revision", current_counts, &allowed)?;
        compare_clippy_previous_revision(workspace, &revision, &previous.clippy_configuration.entries)
    }

    fn compare_policy(&self, previous: &Self) -> Result<()> {
        if self.model.adoption_commit != previous.model.adoption_commit
            || self.model.source_baselines != previous.model.source_baselines
            || self.model.source_governance != previous.model.source_governance
            || self.model.clippy_configuration_path != previous.model.clippy_configuration_path
            || self.source_baseline != previous.source_baseline
        {
            bail!("lint-suppression recovery baseline and governance evidence are immutable");
        }
        require_append_only_paths("Cargo allowance fragments", &self.model.cargo_allowances_paths, &previous.model.cargo_allowances_paths)?;
        compare_source_exceptions(&self.model.source_exceptions, &previous.model.source_exceptions)?;
        compare_cargo_allowances(&self.cargo_allowances.entries, &previous.cargo_allowances.entries)?;
        compare_clippy_settings(&self.clippy_configuration.entries, &previous.clippy_configuration.entries)
    }
}

fn load_revision(workspace: &Path, revision: &str) -> Result<Option<SuppressionPolicy>> {
    let object = format!("{revision}:{POLICY_PATH}");
    let status = Command::new("git")
        .current_dir(workspace)
        .args(["cat-file", "-e", &object])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("inspect lint-suppression policy in maintainability base revision")?;
    if !status.success() {
        verify_commit(workspace, revision)?;
        return Ok(None);
    }
    let model: Policy = git_show_json(workspace, revision, POLICY_PATH)?;
    validate_model(&model)?;
    let source_baseline = collect_baselines(&model.source_baselines, |path| git_show_json::<SourceBaseline>(workspace, revision, path))?;
    let cargo_allowances = super::load_cargo_allowance_files(&model, |path| git_show_json::<CargoAllowanceFile>(workspace, revision, path))?;
    let clippy_configuration = git_show_json::<ClippyConfigurationFile>(workspace, revision, &model.clippy_configuration_path)?;
    SuppressionPolicy::assemble(model, source_baseline, cargo_allowances, clippy_configuration).map(Some)
}

fn git_show_json<T: DeserializeOwned>(workspace: &Path, revision: &str, path: &str) -> Result<T> {
    validate_policy_relative(path)?;
    let object = format!("{revision}:{path}");
    let output = Command::new("git")
        .current_dir(workspace)
        .args(["show", "--no-ext-diff", &object])
        .output()
        .with_context(|| format!("read lint-suppression policy {path:?} from base revision"))?;
    if !output.status.success() {
        bail!("lint-suppression policy {path:?} is unreadable in base revision");
    }
    serde_json::from_slice(&output.stdout).with_context(|| format!("parse lint-suppression policy {path:?} from base revision"))
}

fn verify_commit(workspace: &Path, revision: &str) -> Result<()> {
    let status = Command::new("git")
        .current_dir(workspace)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("verify lint-suppression base revision")?;
    if !status.success() {
        bail!("lint-suppression base revision {revision:?} is not a commit");
    }
    Ok(())
}

fn compare_source_exceptions(current: &[SourceException], previous: &[SourceException]) -> Result<()> {
    if current.len() < previous.len() {
        bail!("lint-suppression source exception evidence is append-only");
    }
    for (current, previous) in current.iter().zip(previous) {
        let mut expected = previous.clone();
        expected.status = current.status;
        if *current != expected || !valid_status_transition(previous.status, current.status) {
            bail!("existing lint-suppression source exception evidence is immutable and cannot reactivate");
        }
    }
    if current.iter().skip(previous.len()).any(|entry| entry.status != Status::Active) {
        bail!("new lint-suppression source exceptions must be active");
    }
    Ok(())
}

fn compare_cargo_allowances(current: &[CargoAllowance], previous: &[CargoAllowance]) -> Result<()> {
    if current.len() < previous.len() {
        bail!("Cargo lint allowance evidence is append-only");
    }
    for (current, previous) in current.iter().zip(previous) {
        let mut expected = previous.clone();
        expected.status = current.status;
        if *current != expected || !valid_status_transition(previous.status, current.status) {
            bail!("existing Cargo lint allowance evidence is immutable and cannot reactivate");
        }
    }
    if current.iter().skip(previous.len()).any(|entry| entry.status != Status::Active) {
        bail!("new Cargo lint allowances must be active");
    }
    Ok(())
}

fn compare_clippy_settings(current: &[ClippySetting], previous: &[ClippySetting]) -> Result<()> {
    if current != previous {
        bail!("Clippy configuration policy key set and evidence are immutable");
    }
    Ok(())
}

fn require_append_only_paths(label: &str, current: &[String], previous: &[String]) -> Result<()> {
    if current.len() < previous.len() || current.iter().zip(previous).any(|(current, previous)| current != previous) {
        bail!("{label} are append-only and existing paths cannot change");
    }
    Ok(())
}

const fn valid_status_transition(previous: Status, current: Status) -> bool {
    matches!((previous, current), (Status::Active, Status::Active | Status::Retired) | (Status::Retired, Status::Retired))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retirement_is_one_way() {
        assert!(valid_status_transition(Status::Active, Status::Active));
        assert!(valid_status_transition(Status::Active, Status::Retired));
        assert!(valid_status_transition(Status::Retired, Status::Retired));
        assert!(!valid_status_transition(Status::Retired, Status::Active));

        let previous = vec!["first.json".to_owned()];
        require_append_only_paths("fragments", &["first.json".to_owned(), "second.json".to_owned()], &previous).expect("append a new fragment");
        assert!(require_append_only_paths("fragments", &["renamed.json".to_owned()], &previous).is_err());

        let setting = ClippySetting {
            id: "clippy.threshold".to_owned(),
            key: "too-many-lines-threshold".to_owned(),
            constraint: super::super::model::ClippyConstraint::MaximumInteger { value: 100 },
            owner: "maintainers".to_owned(),
            issue: "issue".to_owned(),
            pull_request: "pull request".to_owned(),
            rationale: "visible debt".to_owned(),
            safety_invariant: "cannot rise".to_owned(),
            alternatives_considered: "default rejected".to_owned(),
            sentinel: "Clippy".to_owned(),
            evidence: "inventory".to_owned(),
            re_review_phase: "Phase 1".to_owned(),
        };
        assert!(compare_clippy_settings(std::slice::from_ref(&setting), std::slice::from_ref(&setting)).is_ok());
        assert!(compare_clippy_settings(&[setting.clone(), setting], &[]).is_err());
    }
}
