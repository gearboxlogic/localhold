use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Component, Path};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::classify::{FileMeasurement, Inventory};

const CURRENT_SCHEMA_VERSION: u32 = 1;
const BASE_REVISION_ENV: &str = "LOCALHOLD_MAINTAINABILITY_BASE_REV";
const POLICY_PATH: &str = "policy/maintainability/concrete-stores.json";
const UNRESTRICTED_COMPONENTS: [&str; 8] = [
    "composition",
    "context-governance",
    "doctor",
    "migration-schema",
    "persistence-core",
    "postgres-store",
    "sqlite-store",
    "ui",
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConcreteStorePolicy {
    schema_version: u32,
    baseline_commit: String,
    unrestricted_components: Vec<String>,
    debt: Vec<ConcreteStoreDebt>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ConcreteStoreDebt {
    id: String,
    component: String,
    path: String,
    store: ConcreteStoreName,
    baseline_count: usize,
    current_count: usize,
    owner: String,
    issue: String,
    rationale: String,
    resolution_phase: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
enum ConcreteStoreName {
    SqliteStore,
    PostgresStore,
}

type ObservedStore<'a> = (String, &'a str, ConcreteStoreName, usize);

impl ConcreteStorePolicy {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read concrete-store policy {}", path.display()))?;
        let policy: Self = serde_json::from_slice(&bytes).with_context(|| format!("parse concrete-store policy {}", path.display()))?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn compare_current(&self, inventory: &Inventory, component_paths: &BTreeMap<&str, &str>) -> Result<()> {
        self.compare_inventory("current", inventory, component_paths, |debt| debt.current_count)
    }

    pub fn compare_baseline(&self, inventory: &Inventory, component_paths: &BTreeMap<&str, &str>) -> Result<()> {
        self.compare_inventory("baseline", inventory, component_paths, |debt| debt.baseline_count)
    }

    pub fn compare_site_fingerprints(
        &self,
        current: &Inventory,
        baseline: &Inventory,
        current_component_paths: &BTreeMap<&str, &str>,
        baseline_component_paths: &BTreeMap<&str, &str>,
    ) -> Result<()> {
        let unrestricted = self.unrestricted_components.iter().map(String::as_str).collect::<BTreeSet<_>>();
        let debt_components = self
            .debt
            .iter()
            .map(|debt| ((debt.path.clone(), debt.store), debt.component.clone()))
            .collect::<BTreeMap<_, _>>();
        let current_sites = site_fingerprints(current, current_component_paths, Some(&unrestricted), Some(&debt_components), false)?;
        let baseline_sites = site_fingerprints(baseline, baseline_component_paths, Some(&unrestricted), Some(&debt_components), false)?;
        require_occurrence_subset("restricted concrete-store syntax", &current_sites, &baseline_sites)?;

        let current_defaults = site_fingerprints(current, current_component_paths, None, None, true)?;
        let baseline_defaults = site_fingerprints(baseline, baseline_component_paths, None, None, true)?;
        require_occurrence_subset("concrete-store generic default", &current_defaults, &baseline_defaults)
    }

    pub fn require_baseline_commit(&self, expected: &str) -> Result<()> {
        if self.baseline_commit != expected {
            bail!("concrete-store and structure policies must use the same baseline commit");
        }
        Ok(())
    }

    pub fn compare_previous_revision(&self, workspace: &Path) -> Result<()> {
        let Ok(revision) = env::var(BASE_REVISION_ENV) else {
            return Ok(());
        };
        if revision.is_empty() || revision.len() == 40 && revision.bytes().all(|byte| byte == b'0') {
            return Ok(());
        }
        validate_revision(&revision)?;
        let object = format!("{revision}:{POLICY_PATH}");
        let output = Command::new("git")
            .current_dir(workspace)
            .args(["show", "--no-ext-diff", &object])
            .output()
            .context("read concrete-store policy from maintainability base revision")?;
        if !output.status.success() {
            verify_initial_policy_revision(workspace, &revision, &object)?;
            return self.validate_initial_policy();
        }
        let previous: Self = serde_json::from_slice(&output.stdout).context("parse concrete-store policy from maintainability base revision")?;
        previous.validate()?;
        self.compare_policy(&previous)
    }

    fn compare_inventory(&self, label: &str, inventory: &Inventory, component_paths: &BTreeMap<&str, &str>, expected_count: impl Fn(&ConcreteStoreDebt) -> usize) -> Result<()> {
        let unrestricted = self.unrestricted_components.iter().map(String::as_str).collect::<BTreeSet<_>>();
        let mut observed = Vec::new();
        for file in &inventory.files {
            let assigned_component = component_paths
                .get(file.path.as_str())
                .with_context(|| format!("concrete-store {label} inventory path {:?} has no logical component", file.path))?;
            self.record_observed_file(file, assigned_component, &unrestricted, &mut observed);
        }
        observed.sort_unstable();

        let mut expected = self
            .debt
            .iter()
            .filter_map(|debt| {
                let count = expected_count(debt);
                (count > 0).then(|| (debt.component.clone(), debt.path.as_str(), debt.store, count))
            })
            .collect::<Vec<_>>();
        expected.sort_unstable();
        if observed != expected {
            bail!("concrete-store {label} production-name mismatch: expected={expected:?}, observed={observed:?}");
        }
        Ok(())
    }

    fn record_observed_file<'a>(&self, file: &'a FileMeasurement, assigned_component: &str, unrestricted: &BTreeSet<&str>, observed: &mut Vec<ObservedStore<'a>>) {
        for (store, count) in [
            (ConcreteStoreName::SqliteStore, file.production_concrete_stores.sqlite_store),
            (ConcreteStoreName::PostgresStore, file.production_concrete_stores.postgres_store),
        ] {
            if count == 0 {
                continue;
            }
            let debt_component = self
                .debt
                .iter()
                .find(|debt| debt.path == file.path && debt.store == store)
                .map(|debt| debt.component.as_str());
            if debt_component.is_none() && unrestricted.contains(assigned_component) {
                continue;
            }
            observed.push((debt_component.unwrap_or(assigned_component).to_owned(), file.path.as_str(), store, count));
        }
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            bail!("unsupported concrete-store policy schema {}", self.schema_version);
        }
        validate_revision(&self.baseline_commit)?;
        let unrestricted = self.unrestricted_components.iter().map(String::as_str).collect::<BTreeSet<_>>();
        if unrestricted.len() != self.unrestricted_components.len() || unrestricted != UNRESTRICTED_COMPONENTS.into_iter().collect() {
            bail!("concrete-store unrestricted components must remain the exact reviewed persistence/composition set");
        }

        let mut ids = BTreeSet::new();
        let mut sites = BTreeSet::new();
        for debt in &self.debt {
            validate_id(&debt.id)?;
            if !ids.insert(debt.id.as_str()) {
                bail!("duplicate concrete-store debt ID {:?}", debt.id);
            }
            validate_component(&debt.component)?;
            if unrestricted.contains(debt.component.as_str()) {
                bail!("concrete-store debt {:?} is redundant inside unrestricted component {:?}", debt.id, debt.component);
            }
            validate_relative_rust_path(&debt.path)?;
            if !sites.insert((debt.component.as_str(), debt.path.as_str(), debt.store)) {
                bail!("duplicate concrete-store debt for {:?} in {:?}", debt.store, debt.path);
            }
            if debt.baseline_count == 0 {
                bail!("concrete-store debt {:?} baseline count must be positive", debt.id);
            }
            if debt.current_count > debt.baseline_count {
                bail!("concrete-store debt {:?} current count exceeds its recovery baseline", debt.id);
            }
            require_text(&debt.id, "owner", &debt.owner)?;
            require_text(&debt.id, "issue", &debt.issue)?;
            require_text(&debt.id, "rationale", &debt.rationale)?;
            require_text(&debt.id, "resolution phase", &debt.resolution_phase)?;
        }
        Ok(())
    }

    fn compare_policy(&self, previous: &Self) -> Result<()> {
        if self.baseline_commit != previous.baseline_commit {
            bail!("concrete-store baseline commit is immutable");
        }
        if self.unrestricted_components != previous.unrestricted_components {
            bail!("concrete-store unrestricted components are immutable");
        }
        if self.debt.len() != previous.debt.len() {
            bail!("new concrete-store debt is prohibited and recovery debt evidence cannot be removed");
        }
        for (current, previous) in self.debt.iter().zip(&previous.debt) {
            let mut expected = previous.clone();
            expected.current_count = current.current_count;
            if *current != expected {
                bail!("concrete-store debt evidence is immutable except for its downward current-count ratchet");
            }
            if current.current_count > previous.current_count {
                bail!("concrete-store debt {:?} current count cannot increase or resurrect", current.id);
            }
        }
        Ok(())
    }

    fn validate_initial_policy(&self) -> Result<()> {
        if self.debt.iter().any(|debt| debt.current_count != debt.baseline_count) {
            bail!("initial concrete-store policy current counts must equal recovery-baseline counts");
        }
        Ok(())
    }
}

type SiteFingerprint = (String, String, ConcreteStoreName, String);
type DebtComponents = BTreeMap<(String, ConcreteStoreName), String>;

fn site_fingerprints(
    inventory: &Inventory,
    component_paths: &BTreeMap<&str, &str>,
    excluded_components: Option<&BTreeSet<&str>>,
    debt_components: Option<&DebtComponents>,
    generic_defaults: bool,
) -> Result<BTreeMap<SiteFingerprint, usize>> {
    let mut sites = BTreeMap::new();
    for file in &inventory.files {
        let component = component_paths
            .get(file.path.as_str())
            .with_context(|| format!("concrete-store syntax inventory path {:?} has no logical component", file.path))?;
        let source = if generic_defaults {
            &file.production_generic_default_store_sites
        } else {
            &file.production_concrete_store_sites
        };
        for (store, fingerprints) in [
            (ConcreteStoreName::SqliteStore, &source.sqlite_store),
            (ConcreteStoreName::PostgresStore, &source.postgres_store),
        ] {
            let effective_component = debt_components
                .and_then(|components| components.get(&(file.path.clone(), store)))
                .map_or(*component, String::as_str);
            if excluded_components.is_some_and(|excluded| excluded.contains(effective_component)) {
                continue;
            }
            for fingerprint in fingerprints {
                let key = (effective_component.to_owned(), file.path.clone(), store, fingerprint.clone());
                let count = sites.entry(key).or_insert(0_usize);
                *count = count.checked_add(1).context("concrete-store syntax-site occurrence count overflow")?;
            }
        }
    }
    Ok(sites)
}

fn require_occurrence_subset(label: &str, current: &BTreeMap<SiteFingerprint, usize>, baseline: &BTreeMap<SiteFingerprint, usize>) -> Result<()> {
    for (site, current_count) in current {
        let baseline_count = baseline.get(site).copied().unwrap_or_default();
        if *current_count > baseline_count {
            bail!("{label} moved or changed outside its reviewed recovery-baseline site: site={site:?}");
        }
    }
    Ok(())
}

fn validate_id(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_'))
    {
        bail!("concrete-store debt ID must use lowercase ASCII letters, digits, '.', '-', or '_'");
    }
    Ok(())
}

fn validate_component(value: &str) -> Result<()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')) {
        bail!("concrete-store component must use lowercase ASCII letters, digits, '-', or '_'");
    }
    Ok(())
}

fn validate_relative_rust_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    if path.is_absolute()
        || path.extension().and_then(|extension| extension.to_str()) != Some("rs")
        || path.components().any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("concrete-store policy path must be a normalized relative Rust path: {value:?}");
    }
    Ok(())
}

fn validate_revision(revision: &str) -> Result<()> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("concrete-store baseline revision must be a full Git commit hash");
    }
    Ok(())
}

fn require_text(id: &str, label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("concrete-store debt {id:?} {label} must not be empty");
    }
    Ok(())
}

fn verify_initial_policy_revision(workspace: &Path, revision: &str, object: &str) -> Result<()> {
    let status = Command::new("git")
        .current_dir(workspace)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("verify maintainability base revision")?;
    if !status.success() {
        bail!("maintainability base revision {revision:?} is not a commit");
    }
    let object_status = Command::new("git")
        .current_dir(workspace)
        .args(["cat-file", "-e", object])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("inspect concrete-store policy in maintainability base revision")?;
    if object_status.success() {
        bail!("concrete-store policy exists in base revision but could not be read");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
