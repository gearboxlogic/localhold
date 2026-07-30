use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::classify::Inventory;
use super::manifest::PreviousRevision;
use super::revision::maintainability_base_revision;
use super::syntax::VisibilityCounts;

const CURRENT_SCHEMA_VERSION: u32 = 1;
const POLICY_PATH: &str = "policy/maintainability/visibilities.json";
const PHASE_ZERO_ISSUE: &str = "https://github.com/gearboxlogic/localhold/issues/124";
type ExceptionDeltas<'a> = BTreeMap<(&'a str, VisibilityKind), usize>;

mod scope;
mod validation;
use validation::validate_revision;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VisibilityPolicy {
    schema_version: u32,
    baseline_commit: String,
    components: Vec<ComponentVisibility>,
    exceptions: Vec<VisibilityException>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ComponentVisibility {
    component: String,
    baseline: VisibilityBudget,
    current: VisibilityBudget,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct VisibilityBudget {
    pub_crate: usize,
    pub_super: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct VisibilityException {
    id: String,
    component: String,
    kind: VisibilityKind,
    delta: usize,
    scope: VisibilityScope,
    subtree: Option<String>,
    owner: String,
    issue: String,
    pull_request: String,
    rationale: String,
    review_phase: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
enum VisibilityKind {
    PubCrate,
    PubSuper,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum VisibilityScope {
    CrossComponent,
    ComponentSubtree,
}

impl VisibilityPolicy {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read visibility policy {}", path.display()))?;
        let policy: Self = serde_json::from_slice(&bytes).with_context(|| format!("parse visibility policy {}", path.display()))?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn require_baseline_commit(&self, expected: &str) -> Result<()> {
        if self.baseline_commit != expected {
            bail!("visibility and structure policies must use the same baseline commit");
        }
        Ok(())
    }

    pub fn compare_current(&self, inventory: &Inventory, component_paths: &BTreeMap<&str, &str>) -> Result<()> {
        self.compare_inventory("current", inventory, component_paths, |component| component.current)
    }

    pub fn compare_baseline(&self, inventory: &Inventory, component_paths: &BTreeMap<&str, &str>) -> Result<()> {
        self.compare_inventory("baseline", inventory, component_paths, |component| component.baseline)
    }

    pub fn compare_previous_revision(
        &self,
        workspace: &Path,
        current: &Inventory,
        current_paths: &BTreeMap<&str, &str>,
        previous_revision: Option<&PreviousRevision>,
    ) -> Result<()> {
        let Some(revision) = maintainability_base_revision()? else {
            return Ok(());
        };
        validate_revision(&revision)?;
        let object = format!("{revision}:{POLICY_PATH}");
        let output = crate::structure::revision::git_command()
            .current_dir(workspace)
            .args(["show", "--no-ext-diff", &object])
            .output()
            .context("read visibility policy from maintainability base revision")?;
        if !output.status.success() {
            verify_initial_policy_revision(workspace, &revision, &object)?;
            return self.validate_initial_policy();
        }
        let previous: Self = serde_json::from_slice(&output.stdout).context("parse visibility policy from maintainability base revision")?;
        previous.validate()?;
        self.compare_policy(&previous)?;
        let previous_revision = previous_revision.context("visibility policy has previous-revision evidence but the structure inventory does not")?;
        let previous_paths = previous_revision.manifest.current_component_paths()?;
        previous
            .compare_current(&previous_revision.inventory, &previous_paths)
            .context("verify visibility policy evidence from maintainability base revision")?;
        self.compare_scope_evolution((current, current_paths), (&previous_revision.inventory, &previous_paths), &previous)
    }

    fn compare_inventory(
        &self,
        label: &str,
        inventory: &Inventory,
        component_paths: &BTreeMap<&str, &str>,
        expected: impl Fn(&ComponentVisibility) -> VisibilityBudget,
    ) -> Result<()> {
        let policy_components = self.components.iter().map(|component| component.component.as_str()).collect::<BTreeSet<_>>();
        let mapped_components = component_paths.values().copied().collect::<BTreeSet<_>>();
        if policy_components != mapped_components {
            bail!("visibility {label} component set mismatch: policy={policy_components:?}, mapped={mapped_components:?}");
        }

        let mut observed = policy_components
            .iter()
            .map(|component| (*component, VisibilityBudget::default()))
            .collect::<BTreeMap<_, _>>();
        for file in &inventory.files {
            let component = component_paths
                .get(file.path.as_str())
                .with_context(|| format!("visibility {label} inventory path {:?} has no logical component", file.path))?;
            let counts = observed
                .get_mut(component)
                .with_context(|| format!("visibility {label} inventory uses unknown component {component:?}"))?;
            counts.add(file.production_visibilities)?;
        }

        let expected = self
            .components
            .iter()
            .map(|component| (component.component.as_str(), expected(component)))
            .collect::<BTreeMap<_, _>>();
        if observed != expected {
            bail!("visibility {label} production-count mismatch: expected={expected:?}, observed={observed:?}");
        }
        Ok(())
    }

    fn compare_policy(&self, previous: &Self) -> Result<()> {
        if self.baseline_commit != previous.baseline_commit {
            bail!("visibility baseline commit is immutable");
        }
        if self.components.len() != previous.components.len() {
            bail!("visibility component identities and baselines are immutable");
        }
        for (current, previous) in self.components.iter().zip(&previous.components) {
            if current.component != previous.component || current.baseline != previous.baseline {
                bail!("visibility component identities and baselines are immutable");
            }
        }
        if self.exceptions.len() < previous.exceptions.len() || self.exceptions[..previous.exceptions.len()] != previous.exceptions {
            bail!("visibility exceptions are append-only and existing evidence is immutable");
        }

        let appended = exception_deltas(&self.exceptions[previous.exceptions.len()..])?;
        for (current, previous) in self.components.iter().zip(&previous.components) {
            compare_component_evolution(current, previous, &appended)?;
        }
        Ok(())
    }
}

impl VisibilityBudget {
    fn add(&mut self, counts: VisibilityCounts) -> Result<()> {
        self.pub_crate = self.pub_crate.checked_add(counts.pub_crate).context("pub(crate) visibility count overflow")?;
        self.pub_super = self.pub_super.checked_add(counts.pub_super).context("pub(super) visibility count overflow")?;
        Ok(())
    }
}

fn exception_deltas(exceptions: &[VisibilityException]) -> Result<ExceptionDeltas<'_>> {
    let mut totals = BTreeMap::new();
    for exception in exceptions {
        let total = totals.entry((exception.component.as_str(), exception.kind)).or_insert(0_usize);
        *total = total.checked_add(exception.delta).context("visibility exception delta overflow")?;
    }
    Ok(totals)
}

fn compare_component_evolution(current: &ComponentVisibility, previous: &ComponentVisibility, appended: &ExceptionDeltas<'_>) -> Result<()> {
    for kind in [VisibilityKind::PubCrate, VisibilityKind::PubSuper] {
        let increase = budget_count(current.current, kind).saturating_sub(budget_count(previous.current, kind));
        let approved = appended.get(&(current.component.as_str(), kind)).copied().unwrap_or_default();
        if increase != approved {
            bail!(
                "visibility increase for component {:?} and {kind:?} must exactly match newly appended exception deltas: increase={increase}, appended={approved}",
                current.component
            );
        }
    }
    Ok(())
}

const fn budget_count(budget: VisibilityBudget, kind: VisibilityKind) -> usize {
    match kind {
        VisibilityKind::PubCrate => budget.pub_crate,
        VisibilityKind::PubSuper => budget.pub_super,
    }
}

fn verify_initial_policy_revision(workspace: &Path, revision: &str, object: &str) -> Result<()> {
    let status = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("verify maintainability base revision")?;
    if !status.success() {
        bail!("maintainability base revision {revision:?} is not a commit");
    }
    let object_status = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["cat-file", "-e", object])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("inspect visibility policy in maintainability base revision")?;
    if object_status.success() {
        bail!("visibility policy exists in base revision but could not be read");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
