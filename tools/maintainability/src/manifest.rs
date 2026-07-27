use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::scan::{SiteKind, UnsafeSite};

mod focused_tests;

const REQUIRED_ROOTS: [&str; 3] = ["src", "tests", "benches"];
type LintSpec = (&'static str, &'static str, &'static str, i64);
const REQUIRED_LINTS: [LintSpec; 3] = [
    ("rust", "unsafe_code", "deny", 1),
    ("rust", "unsafe_op_in_unsafe_fn", "deny", 1),
    ("clippy", "undocumented_unsafe_blocks", "deny", 1),
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsafeManifest {
    schema_version: u32,
    baseline_commit: String,
    tracked_roots: Vec<String>,
    required_lints: Vec<LintRequirement>,
    contracts: Vec<UnsafeContract>,
    sites: Vec<ExpectedSite>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LintRequirement {
    table: String,
    name: String,
    level: String,
    priority: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnsafeContract {
    id: String,
    owner: String,
    status: ContractStatus,
    necessity: String,
    safe_alternatives_tried: Vec<String>,
    invariants: SafetyInvariants,
    caller_preconditions: Vec<String>,
    safe_wrapper_boundary: String,
    focused_tests: Vec<String>,
    dependency_packages: Vec<DependencyPin>,
    root_dependency_specs: BTreeMap<String, RootDependencySpec>,
    build_route: String,
    review_invalidation_triggers: Vec<String>,
    upstream_removal_trigger: String,
    recovery_issue: String,
    review_phase: String,
    proof_debt: Vec<String>,
    operations: Vec<UnsafeOperation>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ContractStatus {
    TemporaryDebt,
    Required,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SafetyInvariants {
    validity: String,
    lifetime: String,
    aliasing: String,
    abi: String,
    threading: String,
    supported_targets: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnsafeOperation {
    id: String,
    kind: String,
    description: String,
    safety_argument: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DependencyPin {
    pub name: String,
    pub version: String,
    pub source: String,
    pub checksum: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RootDependencySpec {
    pub version: String,
    pub default_features: bool,
    pub features: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedSite {
    id: String,
    contract_id: String,
    path: String,
    item: String,
    kind: SiteKind,
    occurrence: u32,
    fingerprint: String,
    boundary_fingerprint: String,
    operation_ids: Vec<String>,
}

impl UnsafeManifest {
    pub fn load(path: &Path, workspace: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read unsafe manifest {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes).with_context(|| format!("parse unsafe manifest {}", path.display()))?;
        manifest.validate()?;
        for contract in &manifest.contracts {
            for reference in &contract.focused_tests {
                focused_tests::validate(workspace, &contract.id, reference)?;
            }
        }
        Ok(manifest)
    }

    pub fn required_roots() -> Vec<String> {
        REQUIRED_ROOTS.into_iter().map(str::to_owned).collect()
    }

    pub fn roots(&self) -> &[String] {
        &self.tracked_roots
    }

    pub fn required_lints(&self) -> impl Iterator<Item = (&str, &str, &str, i64)> {
        self.required_lints
            .iter()
            .map(|lint| (lint.table.as_str(), lint.name.as_str(), lint.level.as_str(), lint.priority))
    }

    pub fn dependency_packages(&self) -> Result<BTreeMap<&str, &DependencyPin>> {
        let mut packages = BTreeMap::new();
        for contract in &self.contracts {
            merge_package_pins(&mut packages, contract)?;
        }
        Ok(packages)
    }

    pub fn root_dependency_specs(&self) -> Result<BTreeMap<&str, &RootDependencySpec>> {
        let mut specifications = BTreeMap::new();
        for contract in &self.contracts {
            merge_root_dependency_specs(&mut specifications, contract)?;
        }
        Ok(specifications)
    }

    pub fn compare_sites(&self, actual: &[UnsafeSite]) -> Result<()> {
        let expected: BTreeMap<_, _> = self.sites.iter().map(|site| (site.locator(), site)).collect();
        let observed: BTreeMap<_, _> = actual.iter().map(|site| (site.locator(), site)).collect();
        let missing: Vec<_> = expected.keys().filter(|locator| !observed.contains_key(*locator)).collect();
        let unexpected: Vec<_> = observed.keys().filter(|locator| !expected.contains_key(*locator)).collect();
        if !missing.is_empty() || !unexpected.is_empty() {
            bail!("unsafe site inventory changed; missing={missing:?}, unexpected={unexpected:?}");
        }
        for (locator, expected_site) in expected {
            let actual_site = observed.get(&locator).context("observed unsafe site disappeared during comparison")?;
            if actual_site.fingerprint != expected_site.fingerprint {
                bail!(
                    "unsafe site {} changed at {locator:?}: expected fingerprint {}, found {}",
                    expected_site.id,
                    expected_site.fingerprint,
                    actual_site.fingerprint
                );
            }
            if actual_site.boundary_fingerprint != expected_site.boundary_fingerprint {
                bail!(
                    "unsafe site {} enclosing boundary changed at {locator:?}: expected fingerprint {}, found {}",
                    expected_site.id,
                    expected_site.boundary_fingerprint,
                    actual_site.boundary_fingerprint
                );
            }
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!("unsupported unsafe manifest schema {}", self.schema_version);
        }
        if self.baseline_commit.len() != 40 || !self.baseline_commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("unsafe manifest baseline_commit must be a full Git commit hash");
        }
        if self.tracked_roots != Self::required_roots() {
            bail!("unsafe manifest must track exactly {REQUIRED_ROOTS:?}");
        }
        let required: BTreeSet<_> = REQUIRED_LINTS.into_iter().collect();
        let configured: BTreeSet<_> = self.required_lints().collect();
        if configured != required || self.required_lints.len() != required.len() {
            bail!("unsafe manifest must require exactly {REQUIRED_LINTS:?}");
        }
        self.validate_contracts_and_sites()
    }

    fn validate_contracts_and_sites(&self) -> Result<()> {
        let mut contract_ids = BTreeSet::new();
        let mut operations = BTreeMap::new();
        for contract in &self.contracts {
            validate_contract(contract)?;
            if !contract_ids.insert(contract.id.as_str()) {
                bail!("duplicate unsafe contract ID {:?}", contract.id);
            }
            register_operations(&mut operations, contract)?;
        }

        let mut site_ids = BTreeSet::new();
        let mut locators = BTreeSet::new();
        let mut referenced_contracts = BTreeSet::new();
        let mut referenced_operations = BTreeSet::new();
        for site in &self.sites {
            validate_site(site, &contract_ids)?;
            if !site_ids.insert(site.id.as_str()) {
                bail!("duplicate unsafe site ID {:?}", site.id);
            }
            if !locators.insert(site.locator()) {
                bail!("duplicate unsafe site locator {:?}", site.locator());
            }
            referenced_contracts.insert(site.contract_id.as_str());
            validate_site_operation_cardinality(site)?;
            validate_site_operations(site, &operations, &mut referenced_operations)?;
        }
        if referenced_contracts.len() != contract_ids.len() {
            bail!("every unsafe contract must be referenced by at least one site");
        }
        if referenced_operations.len() != operations.len() {
            bail!("every unsafe operation must be referenced by exactly one executable site");
        }
        Ok(())
    }
}

fn merge_package_pins<'a>(packages: &mut BTreeMap<&'a str, &'a DependencyPin>, contract: &'a UnsafeContract) -> Result<()> {
    for package in &contract.dependency_packages {
        if let Some(existing) = packages.insert(&package.name, package)
            && existing != package
        {
            bail!("unsafe contracts require conflicting package pins for {}", package.name);
        }
    }
    Ok(())
}

fn merge_root_dependency_specs<'a>(specifications: &mut BTreeMap<&'a str, &'a RootDependencySpec>, contract: &'a UnsafeContract) -> Result<()> {
    for (name, specification) in &contract.root_dependency_specs {
        if let Some(existing) = specifications.insert(name, specification)
            && existing != specification
        {
            bail!("unsafe contracts require conflicting root dependency specifications for {name}");
        }
    }
    Ok(())
}

impl ExpectedSite {
    fn locator(&self) -> (&str, &str, SiteKind, u32) {
        (&self.path, &self.item, self.kind, self.occurrence)
    }
}

fn validate_contract(contract: &UnsafeContract) -> Result<()> {
    validate_id(&contract.id, "contract ID")?;
    require_text(&contract.owner, "contract owner")?;
    match contract.status {
        ContractStatus::TemporaryDebt => require_entries(&contract.proof_debt, "temporary contract proof debt")?,
        ContractStatus::Required => {
            for entry in &contract.proof_debt {
                require_text(entry, "contract proof debt")?;
            }
        }
    }
    require_text(&contract.necessity, "necessity")?;
    require_entries(&contract.safe_alternatives_tried, "safe alternatives tried")?;
    for (name, value) in [
        ("validity invariant", &contract.invariants.validity),
        ("lifetime invariant", &contract.invariants.lifetime),
        ("aliasing invariant", &contract.invariants.aliasing),
        ("ABI invariant", &contract.invariants.abi),
        ("threading invariant", &contract.invariants.threading),
        ("supported-targets invariant", &contract.invariants.supported_targets),
        ("safe wrapper boundary", &contract.safe_wrapper_boundary),
        ("build route", &contract.build_route),
        ("upstream removal trigger", &contract.upstream_removal_trigger),
        ("recovery issue", &contract.recovery_issue),
        ("review phase", &contract.review_phase),
    ] {
        require_text(value, name)?;
    }
    require_entries(&contract.caller_preconditions, "caller preconditions")?;
    require_entries(&contract.focused_tests, "focused tests")?;
    require_entries(&contract.review_invalidation_triggers, "review invalidation triggers")?;
    if contract.operations.is_empty() {
        bail!("unsafe contract {:?} must enumerate its operations", contract.id);
    }
    let mut dependency_names = BTreeSet::new();
    for package in &contract.dependency_packages {
        require_text(&package.name, "dependency name")?;
        require_text(&package.version, "dependency version")?;
        require_text(&package.source, "dependency source")?;
        if package.checksum.len() != 64 || !package.checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("unsafe contract {:?} has an invalid checksum for {}", contract.id, package.name);
        }
        if !dependency_names.insert(package.name.as_str()) {
            bail!("unsafe contract {:?} repeats dependency {}", contract.id, package.name);
        }
    }
    for (name, specification) in &contract.root_dependency_specs {
        require_text(name, "root dependency name")?;
        require_text(&specification.version, "root dependency version requirement")?;
        let mut features = BTreeSet::new();
        for feature in &specification.features {
            require_text(feature, "root dependency feature")?;
            if !features.insert(feature) {
                bail!("unsafe contract {:?} repeats feature {feature:?} for {name}", contract.id);
            }
        }
    }
    let mut operation_ids = BTreeSet::new();
    for operation in &contract.operations {
        validate_id(&operation.id, "operation ID")?;
        require_text(&operation.kind, "operation kind")?;
        require_text(&operation.description, "operation description")?;
        require_text(&operation.safety_argument, "operation safety argument")?;
        if !operation_ids.insert(operation.id.as_str()) {
            bail!("unsafe contract {:?} repeats operation {:?}", contract.id, operation.id);
        }
    }
    Ok(())
}

fn register_operations<'a>(operations: &mut BTreeMap<&'a str, &'a str>, contract: &'a UnsafeContract) -> Result<()> {
    for operation in &contract.operations {
        if operations.insert(&operation.id, &contract.id).is_some() {
            bail!("duplicate unsafe operation ID {:?}", operation.id);
        }
    }
    Ok(())
}

fn validate_site(site: &ExpectedSite, contract_ids: &BTreeSet<&str>) -> Result<()> {
    validate_id(&site.id, "site ID")?;
    validate_relative_rust_path(&site.path)?;
    require_text(&site.item, "site item")?;
    if site.fingerprint.len() != 64 || !site.fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("unsafe site {:?} has an invalid SHA-256 fingerprint", site.id);
    }
    if site.boundary_fingerprint.len() != 64 || !site.boundary_fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("unsafe site {:?} has an invalid boundary SHA-256 fingerprint", site.id);
    }
    if !contract_ids.contains(site.contract_id.as_str()) {
        bail!("unsafe site {:?} references unknown contract {:?}", site.id, site.contract_id);
    }
    Ok(())
}

fn validate_site_operation_cardinality(site: &ExpectedSite) -> Result<()> {
    if site.kind == SiteKind::LintException {
        if !site.operation_ids.is_empty() {
            bail!("safety-lint exception site {:?} cannot own executable operations", site.id);
        }
    } else if site.operation_ids.len() != 1 {
        bail!("executable unsafe site {:?} must identify exactly one operation", site.id);
    }
    Ok(())
}

fn validate_site_operations<'a>(site: &'a ExpectedSite, operations: &BTreeMap<&'a str, &'a str>, referenced: &mut BTreeSet<&'a str>) -> Result<()> {
    let mut local = BTreeSet::new();
    for operation_id in &site.operation_ids {
        if !local.insert(operation_id.as_str()) {
            bail!("unsafe site {:?} repeats operation {:?}", site.id, operation_id);
        }
        let owner = operations
            .get(operation_id.as_str())
            .with_context(|| format!("unsafe site {:?} references unknown operation {operation_id:?}", site.id))?;
        if *owner != site.contract_id {
            bail!("unsafe site {:?} references operation {operation_id:?} owned by another contract", site.id);
        }
        if !referenced.insert(operation_id) {
            bail!("unsafe operation {operation_id:?} is referenced by more than one executable site");
        }
    }
    Ok(())
}

fn validate_id(value: &str, field: &str) -> Result<()> {
    if value.is_empty() || value.len() > 100 || !value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')) {
        bail!("{field} {value:?} must be a lowercase stable ID");
    }
    Ok(())
}

fn validate_relative_rust_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    if path.extension().and_then(|extension| extension.to_str()) != Some("rs")
        || path.is_absolute()
        || path.components().any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe site path {value:?} must be a normalized relative Rust path");
    }
    Ok(())
}

fn require_entries(values: &[String], field: &str) -> Result<()> {
    if values.is_empty() {
        bail!("{field} must not be empty");
    }
    for value in values {
        require_text(value, field)?;
    }
    Ok(())
}

fn require_text(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
