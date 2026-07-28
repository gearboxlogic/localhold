use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Component, Path};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::classify::{FileMeasurement, Inventory};
use super::manifest::PreviousRevision;
use super::syntax::{ConcreteStoreSignatureSite, PublicReexportEvidence};
use crate::scan::syntax_fingerprint;

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
    #[serde(default)]
    canonical_declarations: Vec<ConcreteStoreDeclaration>,
    debt: Vec<ConcreteStoreDebt>,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(deny_unknown_fields)]
struct ConcreteStoreDeclaration {
    component: String,
    path: String,
    store: ConcreteStoreName,
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    baseline_binding_fingerprint: String,
    #[serde(default)]
    current_binding_fingerprint: String,
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

type ObservedStore = (String, String, ConcreteStoreName);
type DeclarationSite = (String, String, ConcreteStoreName, String, String);
type DeclarationInventory = BTreeMap<DeclarationSite, usize>;

#[derive(Clone, Copy)]
pub(super) struct PathAttribution<'a> {
    components: &'a BTreeMap<&'a str, &'a str>,
    canonical: Option<&'a BTreeMap<String, String>>,
    sites: Option<&'a BTreeMap<String, String>>,
}

impl<'a> PathAttribution<'a> {
    pub(super) const fn identity(component_paths: &'a BTreeMap<&'a str, &'a str>) -> Self {
        Self {
            components: component_paths,
            canonical: None,
            sites: None,
        }
    }

    pub(super) const fn with_lineage(
        component_paths: &'a BTreeMap<&'a str, &'a str>,
        canonical_paths: &'a BTreeMap<String, String>,
        site_paths: &'a BTreeMap<String, String>,
    ) -> Self {
        Self {
            components: component_paths,
            canonical: Some(canonical_paths),
            sites: Some(site_paths),
        }
    }

    fn component_for(&self, path: &str) -> Option<&str> {
        self.components.get(path).copied()
    }

    fn canonical_path<'path>(&'path self, path: &'path str) -> &'path str {
        self.canonical.and_then(|paths| paths.get(path)).map_or(path, String::as_str)
    }

    fn site_path<'path>(&'path self, path: &'path str) -> &'path str {
        self.sites.and_then(|paths| paths.get(path)).map_or(path, String::as_str)
    }
}

#[derive(Clone, Copy)]
struct AttributedPath<'a> {
    assigned_component: &'a str,
    canonical_path: &'a str,
}

#[derive(Clone, Copy)]
struct AttributedInventory<'a> {
    inventory: &'a Inventory,
    paths: PathAttribution<'a>,
}

impl ConcreteStorePolicy {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read concrete-store policy {}", path.display()))?;
        let policy: Self = serde_json::from_slice(&bytes).with_context(|| format!("parse concrete-store policy {}", path.display()))?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn compare_current(&self, inventory: &Inventory, paths: PathAttribution<'_>) -> Result<()> {
        self.compare_inventory("current", inventory, paths, |debt| debt.current_count)
    }

    pub fn compare_canonical_declarations(&self, label: &str, inventory: &Inventory, paths: PathAttribution<'_>) -> Result<()> {
        let expected = self
            .canonical_declarations
            .iter()
            .map(|declaration| {
                let binding = match label {
                    "baseline" => &declaration.baseline_binding_fingerprint,
                    "current" => &declaration.current_binding_fingerprint,
                    _ => bail!("unsupported canonical declaration comparison label {label:?}"),
                };
                Ok((
                    (
                        declaration.component.clone(),
                        declaration.path.clone(),
                        declaration.store,
                        declaration.fingerprint.clone(),
                        binding.clone(),
                    ),
                    1_usize,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut observed = DeclarationInventory::new();
        for file in &inventory.files {
            let component = paths
                .component_for(&file.path)
                .with_context(|| format!("concrete-store {label} declaration path {:?} has no logical component", file.path))?;
            for (store, fingerprints) in [
                (ConcreteStoreName::SqliteStore, &file.production_public_concrete_store_structs.sqlite_store),
                (ConcreteStoreName::PostgresStore, &file.production_public_concrete_store_structs.postgres_store),
            ]
            .into_iter()
            .filter(|(_, fingerprints)| !fingerprints.is_empty())
            {
                let binding = canonical_binding_fingerprint(inventory, paths, component, store)?;
                record_declaration_fingerprints(
                    &mut observed,
                    DeclarationContext {
                        component,
                        path: paths.site_path(&file.path),
                        store,
                        binding_fingerprint: &binding,
                    },
                    fingerprints,
                )?;
            }
        }
        if observed != expected {
            bail!("concrete-store {label} canonical declaration mismatch: expected={expected:?}, observed={observed:?}");
        }
        Ok(())
    }

    pub fn compare_baseline(&self, inventory: &Inventory, component_paths: &BTreeMap<&str, &str>) -> Result<()> {
        self.compare_inventory("baseline", inventory, PathAttribution::identity(component_paths), |debt| debt.baseline_count)
    }

    pub fn compare_site_fingerprints(&self, current: &Inventory, baseline: &Inventory, current_paths: PathAttribution<'_>, baseline_paths: PathAttribution<'_>) -> Result<()> {
        self.compare_site_fingerprints_against(
            AttributedInventory {
                inventory: current,
                paths: current_paths,
            },
            AttributedInventory {
                inventory: baseline,
                paths: baseline_paths,
            },
            "recovery-baseline",
        )
    }

    fn compare_site_fingerprints_against(&self, current: AttributedInventory<'_>, reference: AttributedInventory<'_>, reference_label: &str) -> Result<()> {
        let unrestricted = self.unrestricted_components.iter().map(String::as_str).collect::<BTreeSet<_>>();
        let debt_components = self
            .debt
            .iter()
            .map(|debt| ((debt.path.clone(), debt.store), debt.component.clone()))
            .collect::<BTreeMap<_, _>>();
        let current_sites = site_fingerprints(current.inventory, current.paths, Some(&unrestricted), Some(&debt_components), SiteSource::AllSyntax)?;
        let reference_sites = site_fingerprints(reference.inventory, reference.paths, Some(&unrestricted), Some(&debt_components), SiteSource::AllSyntax)?;
        require_occurrence_subset("restricted concrete-store syntax", reference_label, &current_sites, &reference_sites)?;

        let current_defaults = site_fingerprints(current.inventory, current.paths, None, Some(&debt_components), SiteSource::GenericDefaults)?;
        let reference_defaults = site_fingerprints(reference.inventory, reference.paths, None, Some(&debt_components), SiteSource::GenericDefaults)?;
        require_occurrence_subset("concrete-store generic default", reference_label, &current_defaults, &reference_defaults)?;

        let current_signatures = site_fingerprints(current.inventory, current.paths, None, None, SiteSource::Signatures)?;
        let reference_signatures = site_fingerprints(reference.inventory, reference.paths, None, None, SiteSource::Signatures)?;
        require_occurrence_subset("concrete-store-bearing production signature", reference_label, &current_signatures, &reference_signatures)
    }

    pub fn require_baseline_commit(&self, expected: &str) -> Result<()> {
        if self.baseline_commit != expected {
            bail!("concrete-store and structure policies must use the same baseline commit");
        }
        Ok(())
    }

    pub fn compare_previous_revision(
        &self,
        workspace: &Path,
        current: &Inventory,
        current_paths: PathAttribution<'_>,
        previous_structure: Option<&PreviousRevision>,
    ) -> Result<()> {
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
        previous.validate_previous()?;
        self.compare_policy(&previous)?;

        let previous_structure = previous_structure.context("structure policy is unavailable for concrete-store previous-revision site comparison")?;
        let previous_component_paths = previous_structure.manifest.current_component_paths()?;
        let previous_canonical_paths = previous_structure.manifest.canonical_current_paths()?;
        let previous_site_paths = previous_structure.manifest.current_site_paths()?;
        self.compare_site_fingerprints_against(
            AttributedInventory {
                inventory: current,
                paths: current_paths,
            },
            AttributedInventory {
                inventory: &previous_structure.inventory,
                paths: PathAttribution::with_lineage(&previous_component_paths, &previous_canonical_paths, &previous_site_paths),
            },
            "previous-revision",
        )
    }

    fn compare_inventory(&self, label: &str, inventory: &Inventory, paths: PathAttribution<'_>, expected_count: impl Fn(&ConcreteStoreDebt) -> usize) -> Result<()> {
        let unrestricted = self.unrestricted_components.iter().map(String::as_str).collect::<BTreeSet<_>>();
        let mut observed = BTreeMap::new();
        for file in &inventory.files {
            let assigned_component = paths
                .component_for(&file.path)
                .with_context(|| format!("concrete-store {label} inventory path {:?} has no logical component", file.path))?;
            self.record_observed_file(
                file,
                AttributedPath {
                    assigned_component,
                    canonical_path: paths.canonical_path(&file.path),
                },
                &unrestricted,
                &mut observed,
            )?;
        }

        let expected = self
            .debt
            .iter()
            .filter_map(|debt| {
                let count = expected_count(debt);
                (count > 0).then(|| ((debt.component.clone(), debt.path.clone(), debt.store), count))
            })
            .collect::<BTreeMap<_, _>>();
        if observed != expected {
            bail!("concrete-store {label} production-name mismatch: expected={expected:?}, observed={observed:?}");
        }
        Ok(())
    }

    fn record_observed_file(&self, file: &FileMeasurement, path: AttributedPath<'_>, unrestricted: &BTreeSet<&str>, observed: &mut BTreeMap<ObservedStore, usize>) -> Result<()> {
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
                .find(|debt| debt.path == path.canonical_path && debt.store == store)
                .map(|debt| debt.component.as_str());
            if debt_component.is_none() && unrestricted.contains(path.assigned_component) {
                continue;
            }
            let key = (debt_component.unwrap_or(path.assigned_component).to_owned(), path.canonical_path.to_owned(), store);
            let observed_count = observed.entry(key).or_default();
            *observed_count = observed_count.checked_add(count).context("concrete-store production-name count overflow")?;
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        self.validate_with_declaration_adoption(false)
    }

    fn validate_previous(&self) -> Result<()> {
        self.validate_with_declaration_adoption(true)
    }

    fn validate_with_declaration_adoption(&self, allow_missing_declarations: bool) -> Result<()> {
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
        let declarations = self.canonical_declarations.iter().collect::<BTreeSet<_>>();
        if declarations.len() != self.canonical_declarations.len() {
            bail!("duplicate canonical concrete-store declaration");
        }
        for declaration in &self.canonical_declarations {
            validate_declaration(declaration, &unrestricted, allow_missing_declarations)?;
        }
        if self.canonical_declarations.is_empty() && !allow_missing_declarations {
            bail!("canonical concrete-store declarations must not be empty");
        }
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
        if !previous.canonical_declarations.is_empty() {
            compare_canonical_declarations(&self.canonical_declarations, &previous.canonical_declarations)?;
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

#[derive(Clone, Copy)]
struct DeclarationContext<'a> {
    component: &'a str,
    path: &'a str,
    store: ConcreteStoreName,
    binding_fingerprint: &'a str,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SiteSource {
    AllSyntax,
    GenericDefaults,
    Signatures,
}

#[derive(Clone, Copy)]
struct SiteEvidenceContext<'a> {
    component: &'a str,
    path: &'a str,
    store: ConcreteStoreName,
    public_reexports: &'a [String],
}

struct FileSiteEvidenceContext<'a> {
    inventory: &'a Inventory,
    paths: PathAttribution<'a>,
    file: &'a FileMeasurement,
    excluded_components: Option<&'a BTreeSet<&'a str>>,
    debt_components: Option<&'a DebtComponents>,
    component: &'a str,
    canonical_path: &'a str,
    site_path: &'a str,
}

fn record_declaration_fingerprints(observed: &mut DeclarationInventory, context: DeclarationContext<'_>, fingerprints: &[String]) -> Result<()> {
    for fingerprint in fingerprints {
        let key = (
            context.component.to_owned(),
            context.path.to_owned(),
            context.store,
            fingerprint.clone(),
            context.binding_fingerprint.to_owned(),
        );
        let count = observed.entry(key).or_default();
        *count = count.checked_add(1).context("canonical concrete-store declaration count overflow")?;
    }
    Ok(())
}

fn canonical_binding_fingerprint(inventory: &Inventory, paths: PathAttribution<'_>, component: &str, store: ConcreteStoreName) -> Result<String> {
    let mut evidence = Vec::new();
    let mut found_binding = false;
    for file in &inventory.files {
        if paths.component_for(&file.path) != Some(component) {
            continue;
        }
        let fingerprints = match store {
            ConcreteStoreName::SqliteStore => &file.production_store_binding_sites.sqlite_store,
            ConcreteStoreName::PostgresStore => &file.production_store_binding_sites.postgres_store,
        };
        for fingerprint in fingerprints {
            found_binding = true;
            evidence.push(format!("{}:{}:{fingerprint}", paths.site_path(&file.path).len(), paths.site_path(&file.path)));
        }
    }
    if !found_binding {
        bail!("canonical {store:?} declaration in component {component:?} has no implementation or use evidence");
    }
    evidence.sort();
    Ok(syntax_fingerprint(&evidence.join("\0")))
}

fn public_reexport_evidence(inventory: &Inventory, paths: PathAttribution<'_>, target_module: &[String]) -> Vec<String> {
    let reexports = inventory
        .files
        .iter()
        .flat_map(|file| file.production_public_reexports.iter().map(move |reexport| (file, reexport)))
        .collect::<Vec<_>>();
    let mut evidence = reexports
        .iter()
        .filter(|(_, reexport)| reexport_reaches_module(reexport, &reexports, target_module))
        .map(|(file, reexport)| format!("{}:{}:{}", paths.site_path(&file.path).len(), paths.site_path(&file.path), reexport.fingerprint))
        .collect::<Vec<_>>();
    evidence.sort();
    evidence.dedup();
    evidence
}

fn reexport_reaches_module(candidate: &PublicReexportEvidence, reexports: &[(&FileMeasurement, &PublicReexportEvidence)], target_module: &[String]) -> bool {
    let mut pending = vec![candidate.target_path.clone()];
    let mut visited = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        if reexport_applies_to_module(&path, target_module) {
            return true;
        }
        pending.extend(reexports.iter().filter_map(|(_, reexport)| resolve_reexport_target(&path, reexport)));
    }
    false
}

fn resolve_reexport_target(path: &[String], reexport: &PublicReexportEvidence) -> Option<Vec<String>> {
    let exported_glob = reexport.exported_path.last().is_some_and(|segment| segment == "*");
    let exported_prefix = reexport.exported_path.strip_suffix(&["*".to_owned()]).unwrap_or(&reexport.exported_path);
    if !path.starts_with(exported_prefix) || exported_glob && path.len() == exported_prefix.len() {
        return None;
    }
    let target_prefix = reexport.target_path.strip_suffix(&["*".to_owned()]).unwrap_or(&reexport.target_path);
    let mut resolved = target_prefix.to_vec();
    resolved.extend_from_slice(&path[exported_prefix.len()..]);
    (resolved != path).then_some(resolved)
}

fn reexport_applies_to_module(target_path: &[String], module: &[String]) -> bool {
    let target_without_glob = if target_path.last().is_some_and(|segment| segment == "*") {
        &target_path[..target_path.len() - 1]
    } else {
        target_path
    };
    let parent = target_without_glob.split_last().map_or(&[][..], |(_, parent)| parent);
    module == parent || !target_without_glob.is_empty() && module.starts_with(target_without_glob)
}

fn validate_declaration(declaration: &ConcreteStoreDeclaration, unrestricted: &BTreeSet<&str>, allow_missing_fingerprint: bool) -> Result<()> {
    validate_component(&declaration.component)?;
    validate_relative_rust_path(&declaration.path)?;
    match declaration.fingerprint.as_str() {
        "" if !allow_missing_fingerprint => bail!("canonical concrete-store declaration fingerprints must not be empty"),
        "" => {}
        fingerprint => validate_fingerprint(fingerprint)?,
    }
    for binding in [&declaration.baseline_binding_fingerprint, &declaration.current_binding_fingerprint] {
        match binding.as_str() {
            "" if !allow_missing_fingerprint => bail!("canonical concrete-store binding fingerprints must not be empty"),
            "" => {}
            fingerprint => validate_fingerprint(fingerprint)?,
        }
    }
    if !unrestricted.contains(declaration.component.as_str()) {
        bail!("canonical concrete-store declarations must remain inside an unrestricted component");
    }
    Ok(())
}

fn compare_canonical_declarations(current: &[ConcreteStoreDeclaration], previous: &[ConcreteStoreDeclaration]) -> Result<()> {
    if current.len() != previous.len() {
        bail!("canonical concrete-store declarations are immutable");
    }
    for (current, previous) in current.iter().zip(previous) {
        let mut expected = previous.clone();
        if expected.fingerprint.is_empty() {
            expected.fingerprint.clone_from(&current.fingerprint);
        }
        if expected.baseline_binding_fingerprint.is_empty() {
            expected.baseline_binding_fingerprint.clone_from(&current.baseline_binding_fingerprint);
        }
        if expected.current_binding_fingerprint.is_empty() {
            expected.current_binding_fingerprint.clone_from(&current.current_binding_fingerprint);
        }
        if *current != expected {
            bail!("canonical concrete-store declarations are immutable");
        }
    }
    Ok(())
}

fn site_fingerprints(
    inventory: &Inventory,
    paths: PathAttribution<'_>,
    excluded_components: Option<&BTreeSet<&str>>,
    debt_components: Option<&DebtComponents>,
    source: SiteSource,
) -> Result<BTreeMap<SiteFingerprint, usize>> {
    let mut sites = BTreeMap::new();
    for file in &inventory.files {
        let component = paths
            .component_for(&file.path)
            .with_context(|| format!("concrete-store syntax inventory path {:?} has no logical component", file.path))?;
        let canonical_path = paths.canonical_path(&file.path);
        let site_path = paths.site_path(&file.path);
        let context = FileSiteEvidenceContext {
            inventory,
            paths,
            file,
            excluded_components,
            debt_components,
            component,
            canonical_path,
            site_path,
        };
        for store in [ConcreteStoreName::SqliteStore, ConcreteStoreName::PostgresStore] {
            record_store_site_fingerprints(&mut sites, &context, source, store)?;
        }
    }
    Ok(sites)
}

fn record_store_site_fingerprints(
    sites: &mut BTreeMap<SiteFingerprint, usize>,
    file_context: &FileSiteEvidenceContext<'_>,
    source: SiteSource,
    store: ConcreteStoreName,
) -> Result<()> {
    let effective_component = file_context
        .debt_components
        .and_then(|components| components.get(&(file_context.canonical_path.to_owned(), store)))
        .map_or(file_context.component, String::as_str);
    if file_context.excluded_components.is_some_and(|excluded| excluded.contains(effective_component)) {
        return Ok(());
    }
    if source == SiteSource::Signatures {
        return record_signature_site_fingerprints(sites, file_context, effective_component, store);
    }
    let fingerprints = match source {
        SiteSource::AllSyntax => &file_context.file.production_concrete_store_sites,
        SiteSource::GenericDefaults => &file_context.file.production_generic_default_store_sites,
        SiteSource::Signatures => unreachable!("signature sites handled above"),
    };
    let fingerprints = match store {
        ConcreteStoreName::SqliteStore => &fingerprints.sqlite_store,
        ConcreteStoreName::PostgresStore => &fingerprints.postgres_store,
    };
    let context = SiteEvidenceContext {
        component: effective_component,
        path: file_context.site_path,
        store,
        public_reexports: &[],
    };
    for fingerprint in fingerprints {
        record_site_evidence(sites, context, fingerprint)?;
    }
    Ok(())
}

fn record_signature_site_fingerprints(
    sites: &mut BTreeMap<SiteFingerprint, usize>,
    file_context: &FileSiteEvidenceContext<'_>,
    effective_component: &str,
    store: ConcreteStoreName,
) -> Result<()> {
    let signatures = match store {
        ConcreteStoreName::SqliteStore => &file_context.file.production_signature_store_sites.sqlite_store,
        ConcreteStoreName::PostgresStore => &file_context.file.production_signature_store_sites.postgres_store,
    };
    for signature in signatures {
        record_signature_site_fingerprint(sites, file_context, effective_component, store, signature)?;
    }
    Ok(())
}

fn record_signature_site_fingerprint(
    sites: &mut BTreeMap<SiteFingerprint, usize>,
    file_context: &FileSiteEvidenceContext<'_>,
    effective_component: &str,
    store: ConcreteStoreName,
    signature: &ConcreteStoreSignatureSite,
) -> Result<()> {
    let public_reexports = public_reexport_evidence(file_context.inventory, file_context.paths, &signature.module);
    let context = SiteEvidenceContext {
        component: effective_component,
        path: file_context.site_path,
        store,
        public_reexports: &public_reexports,
    };
    record_site_evidence(sites, context, &signature.fingerprint)
}

fn record_site_evidence(sites: &mut BTreeMap<SiteFingerprint, usize>, context: SiteEvidenceContext<'_>, signature: &str) -> Result<()> {
    let evidence = std::iter::once(signature.to_owned()).chain(
        context
            .public_reexports
            .iter()
            .map(|reexport| syntax_fingerprint(&format!("signature:{signature}\0public-reexport:{reexport}"))),
    );
    for fingerprint in evidence {
        let key = (context.component.to_owned(), context.path.to_owned(), context.store, fingerprint);
        let count = sites.entry(key).or_insert(0_usize);
        *count = count.checked_add(1).context("concrete-store syntax-site occurrence count overflow")?;
    }
    Ok(())
}

fn require_occurrence_subset(label: &str, reference_label: &str, current: &BTreeMap<SiteFingerprint, usize>, baseline: &BTreeMap<SiteFingerprint, usize>) -> Result<()> {
    for (site, current_count) in current {
        let baseline_count = baseline.get(site).copied().unwrap_or_default();
        if *current_count > baseline_count {
            bail!("{label} moved or changed outside its reviewed {reference_label} site: site={site:?}");
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

fn validate_fingerprint(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')) {
        bail!("canonical concrete-store declaration fingerprint must be a lowercase SHA-256 digest");
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
