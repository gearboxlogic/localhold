use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component as PathComponent, Path};

use anyhow::{Context, Result, bail};

use super::measure::component_path_map;
use super::model::{
    CURRENT_SCHEMA_VERSION, ComponentTransfer, Hotspot, HotspotStatus, LEGACY_SCHEMA_VERSION, PRODUCTION_FILE_LIMIT, PathEvolution, PathEvolutionKind, PreGateAdjustment,
    StructureManifest, TEST_FILE_LIMIT,
};
use crate::structure::TRACKED_ROOTS;

impl StructureManifest {
    pub(super) fn validate_current(&self) -> Result<()> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            bail!("unsupported current structure manifest schema {}", self.schema_version);
        }
        self.validate_common()
    }

    pub(super) fn validate_previous(&self) -> Result<()> {
        if !matches!(self.schema_version, LEGACY_SCHEMA_VERSION | CURRENT_SCHEMA_VERSION) {
            bail!("unsupported previous structure manifest schema {}", self.schema_version);
        }
        if self.schema_version == LEGACY_SCHEMA_VERSION && (!self.path_evolutions.is_empty() || !self.component_transfers.is_empty()) {
            bail!("legacy structure manifest cannot contain evolution ledgers");
        }
        self.validate_common()
    }

    fn validate_common(&self) -> Result<()> {
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
        self.validate_adjustments()?;
        self.validate_path_evolutions()?;
        self.validate_component_transfers()
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

    fn validate_path_evolutions(&self) -> Result<()> {
        let mut ids = BTreeSet::new();
        let mut graph = BTreeMap::new();
        for evolution in &self.path_evolutions {
            validate_path_evolution(evolution, &mut ids)?;
            add_acyclic_edges(&mut graph, &evolution.sources, &evolution.successors, "path evolution")?;
        }
        Ok(())
    }

    fn validate_component_transfers(&self) -> Result<()> {
        let components: BTreeSet<_> = self.components.iter().map(|component| component.id.as_str()).collect();
        let evolutions: BTreeSet<_> = self.path_evolutions.iter().map(|evolution| evolution.id.as_str()).collect();
        let mut ids = BTreeSet::new();
        let mut graph = BTreeMap::new();
        for transfer in &self.component_transfers {
            validate_component_transfer(transfer, &components, &evolutions, &mut ids)?;
            add_acyclic_edge(
                &mut graph,
                transfer.source_component.as_str(),
                transfer.destination_component.as_str(),
                "component transfer",
            )?;
        }
        Ok(())
    }

    pub(super) fn current_path_map(&self) -> Result<BTreeMap<&str, &str>> {
        component_path_map(&self.components, |component| &component.paths)
    }

    pub(super) fn baseline_path_map(&self) -> Result<BTreeMap<&str, &str>> {
        component_path_map(&self.components, |component| &component.baseline_paths)
    }

    pub(super) fn component_production_adjustment(&self, component: &str) -> Result<usize> {
        self.pre_gate_adjustments
            .iter()
            .filter(|adjustment| adjustment.component == component)
            .try_fold(0usize, |total, adjustment| {
                total.checked_add(adjustment.production_lines).context("component pre-gate production adjustment overflow")
            })
    }

    pub(super) fn hotspot_adjustment(&self, hotspot: &str) -> Result<(usize, usize)> {
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
        if !current_paths.contains_key(successor.as_str()) {
            bail!("hotspot {:?} successor {successor:?} is not in the current structural map", hotspot.id);
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
    require_text(&adjustment.id, "pre-gate adjustment", "issue", &adjustment.issue)?;
    require_text(&adjustment.id, "pre-gate adjustment", "pull request", &adjustment.pull_request)?;
    require_text(&adjustment.id, "pre-gate adjustment", "rationale", &adjustment.rationale)
}

fn validate_path_evolution<'a>(evolution: &'a PathEvolution, ids: &mut BTreeSet<&'a str>) -> Result<()> {
    validate_id(&evolution.id, "path evolution ID")?;
    if !ids.insert(evolution.id.as_str()) {
        bail!("duplicate path evolution ID {:?}", evolution.id);
    }
    validate_local_paths(&evolution.sources, "path evolution source")?;
    validate_local_paths(&evolution.successors, "path evolution successor")?;
    match evolution.kind {
        PathEvolutionKind::Rename if evolution.sources.len() == 1 && evolution.successors.len() == 1 => {}
        PathEvolutionKind::Split | PathEvolutionKind::TestExtraction if evolution.sources.len() == 1 && evolution.successors.len() >= 2 => {}
        _ => bail!("path evolution {:?} has invalid source/successor cardinality for {:?}", evolution.id, evolution.kind),
    }
    require_evolution_evidence(evolution)
}

fn validate_component_transfer<'a>(transfer: &'a ComponentTransfer, components: &BTreeSet<&str>, evolutions: &BTreeSet<&str>, ids: &mut BTreeSet<&'a str>) -> Result<()> {
    validate_id(&transfer.id, "component transfer ID")?;
    if !ids.insert(transfer.id.as_str()) {
        bail!("duplicate component transfer ID {:?}", transfer.id);
    }
    if transfer.source_component == transfer.destination_component
        || !components.contains(transfer.source_component.as_str())
        || !components.contains(transfer.destination_component.as_str())
    {
        bail!("component transfer {:?} must name distinct existing source and destination components", transfer.id);
    }
    if transfer.production_lines == 0 {
        bail!("component transfer {:?} must move nonzero production lines", transfer.id);
    }
    validate_local_paths(&transfer.paths, "component transfer path")?;
    if !evolutions.contains(transfer.path_evolution.as_str()) {
        bail!("component transfer {:?} references unknown path evolution {:?}", transfer.id, transfer.path_evolution);
    }
    require_text(&transfer.id, "component transfer", "issue", &transfer.issue)?;
    require_text(&transfer.id, "component transfer", "pull request", &transfer.pull_request)?;
    require_text(&transfer.id, "component transfer", "rationale", &transfer.rationale)
}

fn require_evolution_evidence(evolution: &PathEvolution) -> Result<()> {
    require_text(&evolution.id, "path evolution", "issue", &evolution.issue)?;
    require_text(&evolution.id, "path evolution", "pull request", &evolution.pull_request)?;
    require_text(&evolution.id, "path evolution", "rationale", &evolution.rationale)
}

fn require_text(id: &str, kind: &str, label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{kind} {id:?} {label} must not be empty");
    }
    Ok(())
}

fn validate_local_paths(paths: &[String], label: &str) -> Result<()> {
    if paths.is_empty() {
        bail!("{label} list must not be empty");
    }
    let mut local = BTreeSet::new();
    for path in paths {
        validate_relative_rust_path(path)?;
        if !local.insert(path.as_str()) {
            bail!("{label} list repeats {path:?}");
        }
    }
    Ok(())
}

fn add_acyclic_edges<'a>(graph: &mut BTreeMap<&'a str, BTreeSet<&'a str>>, sources: &'a [String], destinations: &'a [String], label: &str) -> Result<()> {
    for source in sources {
        for destination in destinations {
            if source != destination {
                add_acyclic_edge(graph, source, destination, label)?;
            }
        }
    }
    Ok(())
}

fn add_acyclic_edge<'a>(graph: &mut BTreeMap<&'a str, BTreeSet<&'a str>>, source: &'a str, destination: &'a str, label: &str) -> Result<()> {
    if reaches(graph, destination, source) {
        bail!("{label} from {source:?} to {destination:?} creates a round trip");
    }
    graph.entry(source).or_default().insert(destination);
    Ok(())
}

fn reaches(graph: &BTreeMap<&str, BTreeSet<&str>>, start: &str, target: &str) -> bool {
    let mut pending = vec![start];
    let mut seen = BTreeSet::new();
    while let Some(node) = pending.pop() {
        if node == target {
            return true;
        }
        if seen.insert(node)
            && let Some(destinations) = graph.get(node)
        {
            pending.extend(destinations.iter().copied());
        }
    }
    false
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

pub(super) fn validate_id(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > 100 || !value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')) {
        bail!("{label} {value:?} must be a lowercase stable ID");
    }
    Ok(())
}

pub(super) fn validate_relative_rust_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    if path.is_absolute()
        || path.extension().and_then(|extension| extension.to_str()) != Some("rs")
        || path.components().any(|component| !matches!(component, PathComponent::Normal(_)))
    {
        bail!("structure manifest path {value:?} must be a normalized relative Rust path");
    }
    Ok(())
}

pub(super) fn validate_revision(revision: &str) -> Result<()> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("structure manifest baseline_commit must be a full Git commit hash");
    }
    Ok(())
}
