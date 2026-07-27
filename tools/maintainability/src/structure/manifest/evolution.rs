use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::measure::{ObservedFiles, compare_key_sets, inventory_index, sum_counts, sum_production_lines};
use super::model::{ComponentTransfer, HotspotStatus, PathEvolution, PathEvolutionKind, StructureManifest};
use crate::structure::classify::Inventory;

impl StructureManifest {
    pub(super) fn compare_policy(&self, previous: &Self, previous_inventory: &Inventory, current_inventory: &Inventory) -> Result<()> {
        self.compare_immutable_policy(previous)?;
        let new_evolutions = appended("path evolution", &previous.path_evolutions, &self.path_evolutions)?;
        let new_transfers = appended("component transfer", &previous.component_transfers, &self.component_transfers)?;

        let previous_files = inventory_index(previous_inventory)?;
        let current_files = inventory_index(current_inventory)?;
        self.compare_file_exception_policy(previous, &current_files)?;
        self.compare_path_evolution(previous, &previous_files, &current_files, new_evolutions)?;
        self.compare_transfer_evidence(previous, &current_files, new_evolutions, new_transfers)?;
        self.compare_component_policy(previous, &previous_files, &current_files, new_transfers)?;
        self.compare_hotspot_policy(previous, new_evolutions)
    }

    fn compare_immutable_policy(&self, previous: &Self) -> Result<()> {
        if self.baseline_commit != previous.baseline_commit
            || self.tracked_roots != previous.tracked_roots
            || self.limits != previous.limits
            || self.pre_gate_adjustments != previous.pre_gate_adjustments
        {
            bail!("structural baseline identity, roots, hard limits, and pre-gate adjustments are immutable");
        }
        Ok(())
    }

    fn compare_path_evolution(&self, previous: &Self, previous_files: &ObservedFiles<'_>, current_files: &ObservedFiles<'_>, evolutions: &[PathEvolution]) -> Result<()> {
        let evidence = PathEvidence::new(previous, self)?;
        let mut coverage = PathCoverage::default();

        for evolution in evolutions {
            compare_evolution_record(evolution, &evidence, &mut coverage, previous_files, current_files)?;
        }

        require_exact_coverage("path evolution sources", &evidence.expected_sources, &coverage.covered_sources)?;
        require_exact_coverage("path evolution successors", &evidence.expected_successors, &coverage.covered_successors)
    }

    fn compare_component_policy(&self, previous: &Self, previous_files: &ObservedFiles<'_>, current_files: &ObservedFiles<'_>, transfers: &[ComponentTransfer]) -> Result<()> {
        let current = component_index(self);
        let old = component_index(previous);
        compare_key_sets("component", current.keys(), old.keys())?;
        let mut outgoing = BTreeMap::<&str, usize>::new();
        let mut incoming = BTreeMap::<&str, usize>::new();
        for transfer in transfers {
            checked_accumulate(&mut outgoing, &transfer.source_component, transfer.production_lines)?;
            checked_accumulate(&mut incoming, &transfer.destination_component, transfer.production_lines)?;
        }

        for (id, component) in current {
            let previous_component = old.get(id).context("previous component disappeared")?;
            if component.baseline_paths != previous_component.baseline_paths || component.baseline_production_lines != previous_component.baseline_production_lines {
                bail!("component {id:?} baseline paths and production count are immutable");
            }
            let maximum = previous_component
                .production_ceiling
                .checked_sub(outgoing.get(id).copied().unwrap_or(0))
                .with_context(|| format!("component {id:?} transfer exceeds its production ceiling"))?
                .checked_add(incoming.get(id).copied().unwrap_or(0))
                .context("component transfer production ceiling overflow")?;
            if component.production_ceiling > maximum {
                bail!("component {id:?} production ceiling cannot exceed transfer-adjusted maximum {maximum}");
            }
            let previous_observed = sum_production_lines(&previous_component.paths, previous_files)?;
            if previous_observed != previous_component.production_ceiling {
                bail!("previous component {id:?} policy does not match its measured inventory");
            }
            let current_observed = sum_production_lines(&component.paths, current_files)?;
            if current_observed != component.production_ceiling {
                bail!("current component {id:?} policy does not match its measured inventory");
            }
        }
        Ok(())
    }

    fn compare_transfer_evidence(&self, previous: &Self, current_files: &ObservedFiles<'_>, evolutions: &[PathEvolution], transfers: &[ComponentTransfer]) -> Result<()> {
        let previous_paths = previous.current_path_map()?;
        let current_paths = self.current_path_map()?;
        let evolutions_by_id: BTreeMap<_, _> = evolutions.iter().map(|evolution| (evolution.id.as_str(), evolution)).collect();
        let mut covered = BTreeSet::new();

        for transfer in transfers {
            let evolution = evolutions_by_id
                .get(transfer.path_evolution.as_str())
                .with_context(|| format!("component transfer {:?} must reference a path evolution added in the same change", transfer.id))?;
            let source_component = evolution_source_component(evolution, |source| previous_paths.get(source).copied())?;
            if source_component != transfer.source_component {
                bail!("component transfer {:?} source does not match its path evolution", transfer.id);
            }
            let measured = measure_transfer(transfer, evolution, &current_paths, current_files, &mut covered)?;
            if measured != transfer.production_lines {
                bail!(
                    "component transfer {:?} amount mismatch: declared={}, measured={measured}",
                    transfer.id,
                    transfer.production_lines
                );
            }
        }

        let required = required_transfer_paths(evolutions, &previous_paths, &current_paths, current_files)?;
        require_exact_coverage("component transfer paths", &required, &covered)
    }

    fn compare_hotspot_policy(&self, previous: &Self, evolutions: &[PathEvolution]) -> Result<()> {
        let current = hotspot_index(self);
        let old = hotspot_index(previous);
        compare_key_sets("hotspot", current.keys(), old.keys())?;
        for (id, hotspot) in current {
            let previous_hotspot = old.get(id).context("previous hotspot disappeared")?;
            if hotspot.component != previous_hotspot.component
                || hotspot.kind != previous_hotspot.kind
                || hotspot.baseline_path != previous_hotspot.baseline_path
                || hotspot.baseline_physical_lines != previous_hotspot.baseline_physical_lines
                || hotspot.baseline_production_lines != previous_hotspot.baseline_production_lines
            {
                bail!("hotspot {id:?} verified baseline identity is immutable");
            }
            if hotspot.physical_ceiling > previous_hotspot.physical_ceiling || hotspot.production_ceiling > previous_hotspot.production_ceiling {
                bail!("hotspot {id:?} canonical ceilings cannot increase");
            }
            let expected = evolve_hotspot_successors(&previous_hotspot.successors, evolutions);
            let declared: BTreeSet<_> = hotspot.successors.iter().map(String::as_str).collect();
            if expected != declared {
                bail!("hotspot {id:?} successor lineage mismatch: expected={expected:?}, declared={declared:?}");
            }
            if previous_hotspot.status == HotspotStatus::Resolved && hotspot.status != HotspotStatus::Resolved {
                bail!("resolved hotspot {id:?} cannot be reactivated");
            }
        }
        Ok(())
    }
}

fn appended<'a, T: PartialEq>(label: &str, previous: &[T], current: &'a [T]) -> Result<&'a [T]> {
    if current.len() < previous.len() || current.get(..previous.len()) != Some(previous) {
        bail!("{label} ledger is append-only");
    }
    Ok(&current[previous.len()..])
}

struct PathEvidence {
    previous_paths: BTreeMap<String, String>,
    current_paths: BTreeMap<String, String>,
    expected_sources: BTreeSet<String>,
    expected_successors: BTreeSet<String>,
    historical_paths: BTreeSet<String>,
}

impl PathEvidence {
    fn new(previous: &StructureManifest, current: &StructureManifest) -> Result<Self> {
        let previous_paths = owned_path_map(previous)?;
        let current_paths = owned_path_map(current)?;
        let expected_sources = changed_paths(&previous_paths, &current_paths);
        let expected_successors = changed_paths(&current_paths, &previous_paths);
        let historical_paths = previous
            .path_evolutions
            .iter()
            .flat_map(|evolution| evolution.sources.iter().chain(&evolution.successors))
            .cloned()
            .collect();
        Ok(Self {
            previous_paths,
            current_paths,
            expected_sources,
            expected_successors,
            historical_paths,
        })
    }
}

#[derive(Default)]
struct PathCoverage {
    seen_sources: BTreeSet<String>,
    seen_successors: BTreeSet<String>,
    covered_sources: BTreeSet<String>,
    covered_successors: BTreeSet<String>,
}

fn compare_evolution_record(
    evolution: &PathEvolution,
    evidence: &PathEvidence,
    coverage: &mut PathCoverage,
    previous_files: &ObservedFiles<'_>,
    current_files: &ObservedFiles<'_>,
) -> Result<()> {
    evolution_source_component(evolution, |source| evidence.previous_paths.get(source).map(String::as_str))?;
    let mut changed = false;
    for source in &evolution.sources {
        changed |= compare_evolution_source(evolution, source, evidence, coverage)?;
    }
    for successor in &evolution.successors {
        changed |= compare_evolution_successor(evolution, successor, evidence, coverage)?;
    }
    if !changed {
        bail!("path evolution {:?} does not account for any measured path change", evolution.id);
    }
    reject_untraceable_same_path_move(evolution, evidence, current_files)?;
    compare_evolution_counts(evolution, previous_files, current_files)
}

fn reject_untraceable_same_path_move(evolution: &PathEvolution, evidence: &PathEvidence, current_files: &ObservedFiles<'_>) -> Result<()> {
    let source = &evolution.sources[0];
    let is_same_path_move =
        evolution.kind == PathEvolutionKind::Rename && evolution.successors[0] == *source && evidence.previous_paths.get(source) != evidence.current_paths.get(source);
    if !is_same_path_move {
        return Ok(());
    }
    let source_file = current_files.get(source.as_str()).context("same-path evolution source is missing from current inventory")?;
    if source_file.production_lines == 0 {
        bail!(
            "same-path test-only reassignment {:?} must rename the path so component lineage cannot round-trip",
            evolution.id
        );
    }
    Ok(())
}

fn compare_evolution_source(evolution: &PathEvolution, source: &str, evidence: &PathEvidence, coverage: &mut PathCoverage) -> Result<bool> {
    if !coverage.seen_sources.insert(source.to_owned()) {
        bail!("path evolution source {source:?} is consumed more than once");
    }
    if evidence.expected_sources.contains(source) {
        coverage.covered_sources.insert(source.to_owned());
        return Ok(true);
    }
    if !evolution.successors.iter().any(|successor| successor == source) {
        bail!("path evolution {:?} source {source:?} neither changes ownership nor remains a successor", evolution.id);
    }
    Ok(false)
}

fn compare_evolution_successor(evolution: &PathEvolution, successor: &str, evidence: &PathEvidence, coverage: &mut PathCoverage) -> Result<bool> {
    if !coverage.seen_successors.insert(successor.to_owned()) {
        bail!("path evolution successor {successor:?} is declared more than once");
    }
    let was_active = evidence.previous_paths.contains_key(successor);
    if was_active && !evolution.sources.iter().any(|source| source == successor) {
        bail!("path evolution {:?} cannot merge into existing path {successor:?}", evolution.id);
    }
    if !was_active && evidence.historical_paths.contains(successor) {
        bail!("path evolution {:?} cannot resurrect retired path {successor:?}", evolution.id);
    }
    evidence
        .current_paths
        .get(successor)
        .context("path evolution successor is absent from current structural map")?;
    if evidence.expected_successors.contains(successor) {
        coverage.covered_successors.insert(successor.to_owned());
        return Ok(true);
    }
    if !evolution.sources.iter().any(|source| source == successor) {
        bail!("path evolution {:?} successor {successor:?} does not represent a current path change", evolution.id);
    }
    Ok(false)
}

fn owned_path_map(manifest: &StructureManifest) -> Result<BTreeMap<String, String>> {
    Ok(manifest
        .current_path_map()?
        .into_iter()
        .map(|(path, component)| (path.to_owned(), component.to_owned()))
        .collect())
}

fn changed_paths(first: &BTreeMap<String, String>, second: &BTreeMap<String, String>) -> BTreeSet<String> {
    first.iter().filter_map(|(path, owner)| (second.get(path) != Some(owner)).then_some(path.clone())).collect()
}

fn evolution_source_component<'a>(evolution: &PathEvolution, mut owner_for: impl FnMut(&str) -> Option<&'a str>) -> Result<&'a str> {
    let mut owners = evolution
        .sources
        .iter()
        .map(|source| owner_for(source).with_context(|| format!("path evolution {:?} source {source:?} is not active in the previous revision", evolution.id)));
    let owner = owners.next().context("path evolution has no source")??;
    for candidate in owners {
        if candidate? != owner {
            bail!("path evolution {:?} sources span multiple components", evolution.id);
        }
    }
    Ok(owner)
}

fn compare_evolution_counts(evolution: &PathEvolution, previous: &ObservedFiles<'_>, current: &ObservedFiles<'_>) -> Result<()> {
    let source = sum_counts(&evolution.sources, previous)?;
    let successor = sum_counts(&evolution.successors, current)?;
    match evolution.kind {
        PathEvolutionKind::Rename => {
            if successor != source {
                bail!("rename path evolution {:?} must preserve physical and production counts exactly", evolution.id);
            }
        }
        PathEvolutionKind::Split => {
            if source.production > 0 && adds_test_only_successor(evolution, previous, current)? {
                bail!("split path evolution {:?} adds a test-only successor and must be declared as test-extraction", evolution.id);
            }
            if successor.physical > source.physical || successor.production > source.production {
                bail!("split path evolution {:?} cannot increase physical or production counts", evolution.id);
            }
        }
        PathEvolutionKind::TestExtraction => {
            if source.production == 0 {
                bail!("test extraction {:?} must start from production code", evolution.id);
            }
            if successor.physical > source.physical || successor.production != source.production {
                bail!("test extraction {:?} must preserve production and cannot increase physical lines", evolution.id);
            }
            if !adds_test_only_successor(evolution, previous, current)? {
                bail!("test extraction {:?} must add at least one test-only successor", evolution.id);
            }
        }
    }
    Ok(())
}

fn adds_test_only_successor(evolution: &PathEvolution, previous: &ObservedFiles<'_>, current: &ObservedFiles<'_>) -> Result<bool> {
    for path in &evolution.successors {
        let file = current.get(path.as_str()).with_context(|| format!("path evolution successor {path:?} is missing"))?;
        if !previous.contains_key(path.as_str()) && file.production_lines == 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn measure_transfer(
    transfer: &ComponentTransfer,
    evolution: &PathEvolution,
    current_paths: &BTreeMap<&str, &str>,
    current_files: &ObservedFiles<'_>,
    covered: &mut BTreeSet<String>,
) -> Result<usize> {
    let mut total = 0usize;
    for path in &transfer.paths {
        let production = validate_transfer_path(transfer, evolution, path, current_paths, current_files)?;
        record_transferred_path(covered, path)?;
        total = total.checked_add(production).context("component transfer measured line count overflow")?;
    }
    Ok(total)
}

fn validate_transfer_path(
    transfer: &ComponentTransfer,
    evolution: &PathEvolution,
    path: &str,
    current_paths: &BTreeMap<&str, &str>,
    current_files: &ObservedFiles<'_>,
) -> Result<usize> {
    if !evolution.successors.iter().any(|successor| successor == path) {
        bail!("component transfer {:?} path {path:?} is not a successor of {:?}", transfer.id, evolution.id);
    }
    if current_paths.get(path) != Some(&transfer.destination_component.as_str()) {
        bail!("component transfer {:?} path {path:?} is not owned by its destination", transfer.id);
    }
    let production = current_files
        .get(path)
        .with_context(|| format!("component transfer path {path:?} is missing"))?
        .production_lines;
    if production == 0 {
        bail!("component transfer {:?} path {path:?} carries no production lines", transfer.id);
    }
    Ok(production)
}

fn record_transferred_path(covered: &mut BTreeSet<String>, path: &str) -> Result<()> {
    if !covered.insert(path.to_owned()) {
        bail!("component transfer path {path:?} is transferred more than once");
    }
    Ok(())
}

fn required_transfer_paths(
    evolutions: &[PathEvolution],
    previous_paths: &BTreeMap<&str, &str>,
    current_paths: &BTreeMap<&str, &str>,
    current_files: &ObservedFiles<'_>,
) -> Result<BTreeSet<String>> {
    let mut required = BTreeSet::new();
    for evolution in evolutions {
        let source_component = evolution_source_component(evolution, |source| previous_paths.get(source).copied())?;
        for successor in &evolution.successors {
            let owner = current_paths.get(successor.as_str()).context("path evolution successor has no current owner")?;
            let file = current_files
                .get(successor.as_str())
                .with_context(|| format!("component transfer successor {successor:?} is missing"))?;
            if *owner != source_component && file.production_lines > 0 {
                required.insert(successor.clone());
            }
        }
    }
    Ok(required)
}

fn require_exact_coverage(label: &str, expected: &BTreeSet<String>, declared: &BTreeSet<String>) -> Result<()> {
    if expected != declared {
        let missing: Vec<_> = expected.difference(declared).collect();
        let extra: Vec<_> = declared.difference(expected).collect();
        bail!("{label} do not exactly match measured changes: missing={missing:?}, extra={extra:?}");
    }
    Ok(())
}

fn checked_accumulate<'a>(totals: &mut BTreeMap<&'a str, usize>, key: &'a str, amount: usize) -> Result<()> {
    let total = totals.entry(key).or_default();
    *total = total.checked_add(amount).context("component transfer accounting overflow")?;
    Ok(())
}

fn component_index(manifest: &StructureManifest) -> BTreeMap<&str, &super::model::LogicalComponent> {
    manifest.components.iter().map(|component| (component.id.as_str(), component)).collect()
}

fn hotspot_index(manifest: &StructureManifest) -> BTreeMap<&str, &super::model::Hotspot> {
    manifest.hotspots.iter().map(|hotspot| (hotspot.id.as_str(), hotspot)).collect()
}

fn evolve_hotspot_successors<'a>(previous: &'a [String], evolutions: &'a [PathEvolution]) -> BTreeSet<&'a str> {
    let mut expected: BTreeSet<_> = previous.iter().map(String::as_str).collect();
    for evolution in evolutions {
        if evolution.sources.iter().any(|source| expected.contains(source.as_str())) {
            for source in &evolution.sources {
                expected.remove(source.as_str());
            }
            expected.extend(evolution.successors.iter().map(String::as_str));
        }
    }
    expected
}
