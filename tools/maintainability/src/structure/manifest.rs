use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Component as PathComponent, Path};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::TRACKED_ROOTS;
use super::classify::{FileMeasurement, Inventory};

const SCHEMA_VERSION: u32 = 1;
const PRODUCTION_FILE_LIMIT: usize = 800;
const TEST_FILE_LIMIT: usize = 1_000;
const BASE_REVISION_ENV: &str = "LOCALHOLD_MAINTAINABILITY_BASE_REV";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureManifest {
    schema_version: u32,
    pub baseline_commit: String,
    pub tracked_roots: Vec<String>,
    limits: Limits,
    pre_gate_adjustments: Vec<PreGateAdjustment>,
    components: Vec<LogicalComponent>,
    hotspots: Vec<Hotspot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Limits {
    production_file_physical_lines: usize,
    test_file_physical_lines: usize,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct PreGateAdjustment {
    id: String,
    component: String,
    hotspot: String,
    physical_lines: usize,
    production_lines: usize,
    issue: String,
    pull_request: String,
    rationale: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogicalComponent {
    id: String,
    baseline_paths: Vec<String>,
    paths: Vec<String>,
    baseline_production_lines: usize,
    production_ceiling: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum HotspotKind {
    Production,
    Test,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum HotspotStatus {
    Active,
    Resolved,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hotspot {
    id: String,
    component: String,
    kind: HotspotKind,
    status: HotspotStatus,
    baseline_path: String,
    baseline_physical_lines: usize,
    baseline_production_lines: usize,
    successors: Vec<String>,
    physical_ceiling: usize,
    production_ceiling: usize,
}

impl StructureManifest {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read structure manifest {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes).with_context(|| format!("parse structure manifest {}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn compare_current(&self, inventory: &Inventory) -> Result<()> {
        let observed = inventory_index(inventory)?;
        let mapped = self.current_path_map()?;
        compare_file_sets("current", mapped.keys(), observed.keys())?;
        self.compare_component_counts(&observed)?;
        self.compare_hotspot_counts(&observed)?;
        self.compare_file_limits(&observed)
    }

    pub fn compare_baseline(&self, inventory: &Inventory) -> Result<()> {
        let observed = inventory_index(inventory)?;
        let mapped = self.baseline_path_map()?;
        compare_file_sets("baseline", mapped.keys(), observed.keys())?;
        for component in &self.components {
            let production = sum_production_lines(&component.baseline_paths, &observed)?;
            if production != component.baseline_production_lines {
                bail!(
                    "component {:?} baseline production count changed: manifest={}, observed={production}",
                    component.id,
                    component.baseline_production_lines
                );
            }
            let adjusted_baseline = component
                .baseline_production_lines
                .checked_add(self.component_production_adjustment(&component.id)?)
                .context("component adjusted production baseline overflow")?;
            if component.production_ceiling > adjusted_baseline {
                bail!(
                    "component {:?} production ceiling {} exceeds its verified adjusted baseline {adjusted_baseline}",
                    component.id,
                    component.production_ceiling
                );
            }
        }
        self.compare_baseline_hotspots(&observed)
    }

    pub fn compare_previous_revision(&self, workspace: &Path) -> Result<()> {
        self.compare_previous_revision_from(workspace, env::var(BASE_REVISION_ENV).ok().as_deref())
    }

    fn compare_previous_revision_from(&self, workspace: &Path, revision: Option<&str>) -> Result<()> {
        let Some(revision) = revision else {
            return Ok(());
        };
        if revision.is_empty() || revision.len() == 40 && revision.bytes().all(|byte| byte == b'0') {
            return Ok(());
        }
        validate_revision(revision).context("validate maintainability base revision")?;
        let object = format!("{revision}:policy/maintainability/structure.json");
        let output = Command::new("git")
            .current_dir(workspace)
            .args(["show", "--no-ext-diff", &object])
            .output()
            .context("read structure policy from maintainability base revision")?;
        if !output.status.success() {
            return verify_initial_policy_revision(workspace, revision, &object);
        }
        let previous: Self = serde_json::from_slice(&output.stdout).context("parse structure policy from maintainability base revision")?;
        previous.validate().context("validate structure policy from maintainability base revision")?;
        self.compare_policy(&previous)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            bail!("unsupported structure manifest schema {}", self.schema_version);
        }
        validate_revision(&self.baseline_commit)?;
        let required_roots = TRACKED_ROOTS.into_iter().map(str::to_owned).collect::<Vec<_>>();
        if self.tracked_roots != required_roots {
            bail!("structure manifest must track exactly {required_roots:?}");
        }
        if self.limits.production_file_physical_lines != PRODUCTION_FILE_LIMIT || self.limits.test_file_physical_lines != TEST_FILE_LIMIT {
            bail!("structure limits must remain at {PRODUCTION_FILE_LIMIT} production and {TEST_FILE_LIMIT} test physical lines");
        }
        self.validate_components()?;
        self.validate_hotspots()?;
        self.validate_adjustments()
    }

    fn validate_components(&self) -> Result<()> {
        if self.components.is_empty() {
            bail!("structure manifest must define logical components");
        }
        let mut ids = BTreeSet::new();
        let mut baseline_paths = BTreeSet::new();
        let mut current_paths = BTreeSet::new();
        for component in &self.components {
            validate_id(&component.id, "component ID")?;
            if !ids.insert(component.id.as_str()) {
                bail!("duplicate component ID {:?}", component.id);
            }
            if component.baseline_paths.is_empty() || component.paths.is_empty() {
                bail!("component {:?} must own baseline and current paths", component.id);
            }
            validate_path_list(&component.baseline_paths, &mut baseline_paths, "baseline component path")?;
            validate_path_list(&component.paths, &mut current_paths, "current component path")?;
        }
        Ok(())
    }

    fn validate_hotspots(&self) -> Result<()> {
        let component_ids: BTreeSet<_> = self.components.iter().map(|component| component.id.as_str()).collect();
        let baseline_paths = self.baseline_path_map()?;
        let current_paths = self.current_path_map()?;
        let mut ids = BTreeSet::new();
        let mut hotspot_baselines = BTreeSet::new();
        let mut all_successors = BTreeSet::new();
        for hotspot in &self.hotspots {
            validate_hotspot_identity(hotspot, &component_ids, &baseline_paths, &mut ids, &mut hotspot_baselines)?;
            validate_hotspot_successors(hotspot, &current_paths, &mut all_successors)?;
            self.validate_hotspot_ceilings(hotspot)?;
        }
        Ok(())
    }

    fn validate_hotspot_ceilings(&self, hotspot: &Hotspot) -> Result<()> {
        let (physical_adjustment, production_adjustment) = self.hotspot_adjustment(&hotspot.id)?;
        let physical_limit = hotspot
            .baseline_physical_lines
            .checked_add(physical_adjustment)
            .context("hotspot adjusted physical baseline overflow")?;
        let production_limit = hotspot
            .baseline_production_lines
            .checked_add(production_adjustment)
            .context("hotspot adjusted production baseline overflow")?;
        if hotspot.physical_ceiling > physical_limit || hotspot.production_ceiling > production_limit {
            bail!("hotspot {:?} ceilings cannot exceed its verified baseline counts", hotspot.id);
        }
        Ok(())
    }

    fn validate_adjustments(&self) -> Result<()> {
        let components: BTreeSet<_> = self.components.iter().map(|component| component.id.as_str()).collect();
        let hotspots: BTreeMap<_, _> = self.hotspots.iter().map(|hotspot| (hotspot.id.as_str(), hotspot.component.as_str())).collect();
        let mut ids = BTreeSet::new();
        for adjustment in &self.pre_gate_adjustments {
            validate_adjustment(adjustment, &components, &hotspots, &mut ids)?;
        }
        Ok(())
    }

    fn compare_component_counts(&self, observed: &BTreeMap<&str, &FileMeasurement>) -> Result<()> {
        for component in &self.components {
            let production = sum_production_lines(&component.paths, observed)?;
            if production > component.production_ceiling {
                bail!(
                    "component {:?} production growth rejected: ceiling={}, observed={production}",
                    component.id,
                    component.production_ceiling
                );
            }
            if production < component.production_ceiling {
                bail!(
                    "component {:?} production ceiling must be lowered from {} to {production} in this change",
                    component.id,
                    component.production_ceiling
                );
            }
        }
        Ok(())
    }

    fn compare_hotspot_counts(&self, observed: &BTreeMap<&str, &FileMeasurement>) -> Result<()> {
        for hotspot in &self.hotspots {
            compare_current_hotspot(hotspot, observed)?;
        }
        Ok(())
    }

    fn compare_file_limits(&self, observed: &BTreeMap<&str, &FileMeasurement>) -> Result<()> {
        let active_successors: BTreeSet<_> = self
            .hotspots
            .iter()
            .filter(|hotspot| hotspot.status == HotspotStatus::Active)
            .flat_map(|hotspot| hotspot.successors.iter().map(String::as_str))
            .collect();
        for file in observed.values() {
            let limit = applicable_limit(file);
            if file.physical_lines > limit && !active_successors.contains(file.path.as_str()) {
                bail!(
                    "{} has {} physical lines, exceeds its {limit}-line {} limit, and is not an active baseline hotspot successor",
                    file.path,
                    file.physical_lines,
                    file_kind(file)
                );
            }
        }
        Ok(())
    }

    fn compare_baseline_hotspots(&self, observed: &BTreeMap<&str, &FileMeasurement>) -> Result<()> {
        let expected: BTreeSet<_> = observed
            .values()
            .filter(|file| file.physical_lines > applicable_limit(file))
            .map(|file| file.path.as_str())
            .collect();
        let declared: BTreeSet<_> = self.hotspots.iter().map(|hotspot| hotspot.baseline_path.as_str()).collect();
        if expected != declared {
            bail!("baseline hotspot set mismatch: expected={expected:?}, declared={declared:?}");
        }
        for hotspot in &self.hotspots {
            let file = observed.get(hotspot.baseline_path.as_str()).context("baseline hotspot source is missing")?;
            if file.physical_lines != hotspot.baseline_physical_lines || file.production_lines != hotspot.baseline_production_lines {
                bail!(
                    "hotspot {:?} baseline counts changed: manifest=({},{}) observed=({},{})",
                    hotspot.id,
                    hotspot.baseline_physical_lines,
                    hotspot.baseline_production_lines,
                    file.physical_lines,
                    file.production_lines
                );
            }
            validate_hotspot_kind(hotspot, file.production_lines)?;
        }
        Ok(())
    }

    fn compare_policy(&self, previous: &Self) -> Result<()> {
        if self.schema_version != previous.schema_version
            || self.baseline_commit != previous.baseline_commit
            || self.tracked_roots != previous.tracked_roots
            || self.limits != previous.limits
            || self.pre_gate_adjustments != previous.pre_gate_adjustments
        {
            bail!("structural baseline identity, roots, hard limits, and pre-gate adjustments are immutable");
        }
        let current_components: BTreeMap<_, _> = self.components.iter().map(|component| (component.id.as_str(), component)).collect();
        let previous_components: BTreeMap<_, _> = previous.components.iter().map(|component| (component.id.as_str(), component)).collect();
        compare_key_sets("component", current_components.keys(), previous_components.keys())?;
        for (id, current) in current_components {
            let old = previous_components.get(id).context("previous component disappeared")?;
            if current.baseline_paths != old.baseline_paths || current.baseline_production_lines != old.baseline_production_lines {
                bail!("component {id:?} baseline paths and production count are immutable");
            }
            if current.production_ceiling > old.production_ceiling {
                bail!(
                    "component {id:?} production ceiling cannot increase from {} to {}",
                    old.production_ceiling,
                    current.production_ceiling
                );
            }
        }
        if previous.current_path_map()? != self.current_path_map()? {
            bail!("structural path ownership is immutable until governed successor and transfer support is installed");
        }
        self.compare_hotspot_policy(previous)
    }

    fn compare_hotspot_policy(&self, previous: &Self) -> Result<()> {
        let current: BTreeMap<_, _> = self.hotspots.iter().map(|hotspot| (hotspot.id.as_str(), hotspot)).collect();
        let previous: BTreeMap<_, _> = previous.hotspots.iter().map(|hotspot| (hotspot.id.as_str(), hotspot)).collect();
        compare_key_sets("hotspot", current.keys(), previous.keys())?;
        for (id, current) in current {
            let old = previous.get(id).context("previous hotspot disappeared")?;
            if current.component != old.component
                || current.kind != old.kind
                || current.baseline_path != old.baseline_path
                || current.baseline_physical_lines != old.baseline_physical_lines
                || current.baseline_production_lines != old.baseline_production_lines
            {
                bail!("hotspot {id:?} verified baseline identity is immutable");
            }
            if current.physical_ceiling > old.physical_ceiling || current.production_ceiling > old.production_ceiling {
                bail!("hotspot {id:?} canonical ceilings cannot increase");
            }
            if current.successors != old.successors {
                bail!("hotspot {id:?} successor lineage is immutable until governed split support is installed");
            }
            if old.status == HotspotStatus::Resolved && current.status != HotspotStatus::Resolved {
                bail!("resolved hotspot {id:?} cannot be reactivated");
            }
        }
        Ok(())
    }

    fn current_path_map(&self) -> Result<BTreeMap<&str, &str>> {
        component_path_map(&self.components, |component| &component.paths)
    }

    fn baseline_path_map(&self) -> Result<BTreeMap<&str, &str>> {
        component_path_map(&self.components, |component| &component.baseline_paths)
    }

    fn component_production_adjustment(&self, component: &str) -> Result<usize> {
        self.pre_gate_adjustments
            .iter()
            .filter(|adjustment| adjustment.component == component)
            .try_fold(0usize, |total, adjustment| {
                total.checked_add(adjustment.production_lines).context("component pre-gate production adjustment overflow")
            })
    }

    fn hotspot_adjustment(&self, hotspot: &str) -> Result<(usize, usize)> {
        self.pre_gate_adjustments
            .iter()
            .filter(|adjustment| adjustment.hotspot == hotspot)
            .try_fold((0usize, 0usize), |(physical, production), adjustment| {
                Ok((
                    physical.checked_add(adjustment.physical_lines).context("hotspot pre-gate physical adjustment overflow")?,
                    production
                        .checked_add(adjustment.production_lines)
                        .context("hotspot pre-gate production adjustment overflow")?,
                ))
            })
    }
}

fn validate_hotspot_identity<'a>(
    hotspot: &'a Hotspot,
    component_ids: &BTreeSet<&str>,
    baseline_paths: &BTreeMap<&str, &str>,
    ids: &mut BTreeSet<&'a str>,
    hotspot_baselines: &mut BTreeSet<&'a str>,
) -> Result<()> {
    validate_id(&hotspot.id, "hotspot ID")?;
    if !ids.insert(hotspot.id.as_str()) {
        bail!("duplicate hotspot ID {:?}", hotspot.id);
    }
    if !component_ids.contains(hotspot.component.as_str()) {
        bail!("hotspot {:?} references unknown component {:?}", hotspot.id, hotspot.component);
    }
    validate_relative_rust_path(&hotspot.baseline_path)?;
    if !hotspot_baselines.insert(hotspot.baseline_path.as_str()) {
        bail!("multiple hotspots claim baseline path {:?}", hotspot.baseline_path);
    }
    if baseline_paths.get(hotspot.baseline_path.as_str()) != Some(&hotspot.component.as_str()) {
        bail!("hotspot {:?} baseline path is not owned by component {:?}", hotspot.id, hotspot.component);
    }
    Ok(())
}

fn validate_hotspot_successors<'a>(hotspot: &'a Hotspot, current_paths: &BTreeMap<&str, &str>, all_successors: &mut BTreeSet<&'a str>) -> Result<()> {
    if hotspot.status == HotspotStatus::Active && hotspot.successors.is_empty() {
        bail!("active hotspot {:?} must retain its successor lineage", hotspot.id);
    }
    let mut local = BTreeSet::new();
    for successor in &hotspot.successors {
        validate_relative_rust_path(successor)?;
        if !local.insert(successor.as_str()) {
            bail!("hotspot {:?} repeats successor {successor:?}", hotspot.id);
        }
        if current_paths.get(successor.as_str()) != Some(&hotspot.component.as_str()) {
            bail!("hotspot {:?} successor {successor:?} is not owned by component {:?}", hotspot.id, hotspot.component);
        }
        if !all_successors.insert(successor.as_str()) {
            bail!("hotspot successor {successor:?} belongs to more than one lineage");
        }
    }
    Ok(())
}

fn validate_adjustment<'a>(adjustment: &'a PreGateAdjustment, components: &BTreeSet<&str>, hotspots: &BTreeMap<&str, &str>, ids: &mut BTreeSet<&'a str>) -> Result<()> {
    validate_id(&adjustment.id, "pre-gate adjustment ID")?;
    if !ids.insert(adjustment.id.as_str()) {
        bail!("duplicate pre-gate adjustment ID {:?}", adjustment.id);
    }
    if !components.contains(adjustment.component.as_str()) || hotspots.get(adjustment.hotspot.as_str()) != Some(&adjustment.component.as_str()) {
        bail!("pre-gate adjustment {:?} references an unknown component or hotspot", adjustment.id);
    }
    if adjustment.physical_lines == 0 && adjustment.production_lines == 0 {
        bail!("pre-gate adjustment {:?} must record a nonzero change", adjustment.id);
    }
    if adjustment.production_lines > adjustment.physical_lines {
        bail!("pre-gate adjustment {:?} production lines cannot exceed physical lines", adjustment.id);
    }
    require_adjustment_text(adjustment, "issue", &adjustment.issue)?;
    require_adjustment_text(adjustment, "pull request", &adjustment.pull_request)?;
    require_adjustment_text(adjustment, "rationale", &adjustment.rationale)
}

fn require_adjustment_text(adjustment: &PreGateAdjustment, label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("pre-gate adjustment {:?} {label} must not be empty", adjustment.id);
    }
    Ok(())
}

fn compare_current_hotspot(hotspot: &Hotspot, observed: &BTreeMap<&str, &FileMeasurement>) -> Result<()> {
    let physical = sum_physical_lines(&hotspot.successors, observed)?;
    let production = sum_production_lines(&hotspot.successors, observed)?;
    compare_ratchet("physical", &hotspot.id, hotspot.physical_ceiling, physical)?;
    compare_ratchet("production", &hotspot.id, hotspot.production_ceiling, production)?;
    match hotspot.status {
        HotspotStatus::Active => {
            validate_hotspot_kind(hotspot, production)?;
            if hotspot
                .successors
                .iter()
                .all(|path| observed.get(path.as_str()).is_some_and(|file| file.physical_lines <= applicable_limit(file)))
            {
                bail!("hotspot {:?} is below every successor file limit and must be marked resolved", hotspot.id);
            }
        }
        HotspotStatus::Resolved => validate_resolved_successors(hotspot, observed)?,
    }
    Ok(())
}

fn validate_resolved_successors(hotspot: &Hotspot, observed: &BTreeMap<&str, &FileMeasurement>) -> Result<()> {
    for successor in &hotspot.successors {
        if let Some(file) = observed.get(successor.as_str())
            && file.physical_lines > applicable_limit(file)
        {
            bail!("resolved hotspot {:?} successor {successor:?} exceeds its ordinary file limit", hotspot.id);
        }
    }
    Ok(())
}

fn inventory_index(inventory: &Inventory) -> Result<BTreeMap<&str, &FileMeasurement>> {
    let mut files = BTreeMap::new();
    for file in &inventory.files {
        validate_relative_rust_path(&file.path)?;
        if file.production_lines + file.test_lines != file.physical_lines {
            bail!("structural inventory counts do not sum for {}", file.path);
        }
        if files.insert(file.path.as_str(), file).is_some() {
            bail!("structural inventory repeats {}", file.path);
        }
    }
    Ok(files)
}

fn component_path_map<'a>(components: &'a [LogicalComponent], select: impl Fn(&'a LogicalComponent) -> &'a [String]) -> Result<BTreeMap<&'a str, &'a str>> {
    let mut paths = BTreeMap::new();
    for component in components {
        for path in select(component) {
            if let Some(previous) = paths.insert(path.as_str(), component.id.as_str()) {
                bail!("structural path {path:?} belongs to both {previous:?} and {:?}", component.id);
            }
        }
    }
    Ok(paths)
}

fn compare_file_sets<'a>(label: &str, mapped: impl Iterator<Item = &'a &'a str>, observed: impl Iterator<Item = &'a &'a str>) -> Result<()> {
    let mapped: BTreeSet<_> = mapped.copied().collect();
    let observed: BTreeSet<_> = observed.copied().collect();
    if mapped != observed {
        let missing: Vec<_> = mapped.difference(&observed).copied().collect();
        let unmapped: Vec<_> = observed.difference(&mapped).copied().collect();
        bail!("{label} structural file map mismatch: missing={missing:?}, unmapped={unmapped:?}");
    }
    Ok(())
}

fn compare_key_sets<'a>(label: &str, current: impl Iterator<Item = &'a &'a str>, previous: impl Iterator<Item = &'a &'a str>) -> Result<()> {
    let current: BTreeSet<_> = current.copied().collect();
    let previous: BTreeSet<_> = previous.copied().collect();
    if current != previous {
        bail!("{label} baseline IDs are immutable: current={current:?}, previous={previous:?}");
    }
    Ok(())
}

fn sum_production_lines(paths: &[String], observed: &BTreeMap<&str, &FileMeasurement>) -> Result<usize> {
    paths.iter().try_fold(0usize, |total, path| {
        let count = observed
            .get(path.as_str())
            .with_context(|| format!("structural source {path:?} is missing"))?
            .production_lines;
        total.checked_add(count).context("component production line count overflow")
    })
}

fn sum_physical_lines(paths: &[String], observed: &BTreeMap<&str, &FileMeasurement>) -> Result<usize> {
    paths.iter().try_fold(0usize, |total, path| {
        let count = observed
            .get(path.as_str())
            .with_context(|| format!("structural source {path:?} is missing"))?
            .physical_lines;
        total.checked_add(count).context("hotspot physical line count overflow")
    })
}

fn compare_ratchet(kind: &str, id: &str, ceiling: usize, observed: usize) -> Result<()> {
    if observed > ceiling {
        bail!("hotspot {id:?} {kind} growth rejected: ceiling={ceiling}, observed={observed}");
    }
    if observed < ceiling {
        bail!("hotspot {id:?} {kind} ceiling must ratchet from {ceiling} to {observed} in this change");
    }
    Ok(())
}

fn validate_hotspot_kind(hotspot: &Hotspot, production_lines: usize) -> Result<()> {
    let observed = if production_lines == 0 { HotspotKind::Test } else { HotspotKind::Production };
    if hotspot.kind != observed {
        bail!("hotspot {:?} kind {:?} does not match observed kind {observed:?}", hotspot.id, hotspot.kind);
    }
    Ok(())
}

const fn applicable_limit(file: &FileMeasurement) -> usize {
    if file.production_lines == 0 { TEST_FILE_LIMIT } else { PRODUCTION_FILE_LIMIT }
}

const fn file_kind(file: &FileMeasurement) -> &'static str {
    if file.production_lines == 0 { "test" } else { "production" }
}

fn validate_path_list<'a>(paths: &'a [String], all: &mut BTreeSet<&'a str>, label: &str) -> Result<()> {
    let mut local = BTreeSet::new();
    for path in paths {
        validate_relative_rust_path(path)?;
        if !local.insert(path.as_str()) {
            bail!("{label} list repeats {path:?}");
        }
        if !all.insert(path.as_str()) {
            bail!("{label} {path:?} appears in multiple components");
        }
    }
    Ok(())
}

fn validate_id(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > 100 || !value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')) {
        bail!("{label} {value:?} must be a lowercase stable ID");
    }
    Ok(())
}

fn validate_relative_rust_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    if path.is_absolute()
        || path.extension().and_then(|extension| extension.to_str()) != Some("rs")
        || path.components().any(|component| !matches!(component, PathComponent::Normal(_)))
    {
        bail!("structure manifest path {value:?} must be a normalized relative Rust path");
    }
    Ok(())
}

fn validate_revision(revision: &str) -> Result<()> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("structure manifest baseline_commit must be a full Git commit hash");
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
        bail!("maintainability base revision {revision} is not available");
    }
    let status = Command::new("git")
        .current_dir(workspace)
        .args(["cat-file", "-e", object])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("inspect structure policy in maintainability base revision")?;
    if status.success() {
        bail!("structure policy exists in base revision but could not be read");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
