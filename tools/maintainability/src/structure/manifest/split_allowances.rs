use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

#[cfg(test)]
use super::measure::inventory_index;
use super::measure::{LineCounts, ObservedFiles, sum_counts};
use super::model::{FeatureFreezeStatus, Hotspot, HotspotStatus, PathEvolution, PathEvolutionKind, SplitAllowance, SplitAllowanceStatus, StructureManifest};
use super::validate::{require_text, validate_id};
#[cfg(test)]
use crate::structure::classify::Inventory;

pub(super) struct SplitPolicyEvidence<'a, 'previous, 'current> {
    pub(super) previous_files: &'a ObservedFiles<'previous>,
    pub(super) current_files: &'a ObservedFiles<'current>,
    pub(super) new_evolutions: &'a [PathEvolution],
    pub(super) touched: &'a BTreeSet<String>,
}

impl StructureManifest {
    pub(super) fn validate_split_allowances(&self) -> Result<()> {
        let hotspots = hotspot_index(self);
        let evolutions: BTreeMap<_, _> = self.path_evolutions.iter().map(|evolution| (evolution.id.as_str(), evolution)).collect();
        let current_paths = self.current_path_map()?;
        let mut ids = BTreeSet::new();
        let mut claimed_hotspots = BTreeSet::new();
        let mut claimed_evolutions = BTreeSet::new();
        for allowance in &self.split_allowances {
            validate_id(&allowance.id, "split allowance ID")?;
            if !ids.insert(allowance.id.as_str()) {
                bail!("duplicate split allowance ID {:?}", allowance.id);
            }
            if !claimed_hotspots.insert(allowance.hotspot.as_str()) {
                bail!("hotspot {:?} has more than one split allowance", allowance.hotspot);
            }
            if !claimed_evolutions.insert(allowance.path_evolution.as_str()) {
                bail!("path evolution {:?} has more than one split allowance", allowance.path_evolution);
            }
            let hotspot = hotspots
                .get(allowance.hotspot.as_str())
                .with_context(|| format!("split allowance {:?} references an unknown hotspot", allowance.id))?;
            let evolution = evolutions
                .get(allowance.path_evolution.as_str())
                .with_context(|| format!("split allowance {:?} references an unknown path evolution", allowance.id))?;
            if evolution.kind != PathEvolutionKind::Split {
                bail!("split allowance {:?} must reference a structural split", allowance.id);
            }
            validate_allowance_contract(allowance)?;
            validate_active_ownership(allowance, hotspot, &current_paths)?;
        }
        if self.feature_freeze == FeatureFreezeStatus::Exited && self.split_allowances.iter().any(|allowance| allowance.status == SplitAllowanceStatus::Active) {
            bail!("feature freeze cannot exit with outstanding split overhead");
        }
        Ok(())
    }

    pub(super) fn compare_split_allowance_policy(&self, previous: &Self, evidence: &SplitPolicyEvidence<'_, '_, '_>) -> Result<()> {
        if previous.feature_freeze == FeatureFreezeStatus::Exited && self.feature_freeze != FeatureFreezeStatus::Exited {
            bail!("feature freeze exit is irreversible");
        }
        if self.split_allowances.len() < previous.split_allowances.len() {
            bail!("split allowance ledger is append-only");
        }
        for (prior, current) in previous.split_allowances.iter().zip(&self.split_allowances) {
            compare_existing_allowance(prior, current, previous, self, evidence.touched)?;
        }
        for allowance in &self.split_allowances[previous.split_allowances.len()..] {
            validate_new_allowance(allowance, previous, self, evidence)?;
        }
        Ok(())
    }

    pub(super) fn split_allowance_counts(&self, hotspot: &str) -> LineCounts {
        self.split_allowances
            .iter()
            .find(|allowance| allowance.hotspot == hotspot)
            .map_or(LineCounts { physical: 0, production: 0 }, |allowance| LineCounts {
                physical: allowance.current_physical_lines,
                production: allowance.current_production_lines,
            })
    }

    pub(super) fn component_split_production_allowance(&self, component: &str) -> Result<usize> {
        let hotspots = hotspot_index(self);
        self.split_allowances.iter().try_fold(0usize, |total, allowance| {
            let hotspot = hotspots.get(allowance.hotspot.as_str()).context("validated split allowance lost its hotspot")?;
            if hotspot.component != component {
                return Ok(total);
            }
            total.checked_add(allowance.current_production_lines).context("component split allowance total overflow")
        })
    }

    pub(super) fn new_split_allowance_counts(&self, previous: &Self, evolution: &str) -> Option<LineCounts> {
        self.split_allowances[previous.split_allowances.len()..]
            .iter()
            .find(|allowance| allowance.path_evolution == evolution)
            .map(|allowance| LineCounts {
                physical: allowance.approved_physical_lines,
                production: allowance.approved_production_lines,
            })
    }

    pub(super) fn new_split_production_for_component(&self, previous: &Self, component: &str) -> Result<usize> {
        let hotspots = hotspot_index(self);
        self.split_allowances[previous.split_allowances.len()..].iter().try_fold(0usize, |total, allowance| {
            let hotspot = hotspots.get(allowance.hotspot.as_str()).context("new split allowance lost its hotspot")?;
            if hotspot.component != component {
                return Ok(total);
            }
            total
                .checked_add(allowance.approved_production_lines)
                .context("new component split allowance total overflow")
        })
    }
}

#[cfg(test)]
pub(super) fn changed_inventory_paths(previous: &Inventory, current: &Inventory) -> Result<BTreeSet<String>> {
    let previous = inventory_index(previous)?;
    let current = inventory_index(current)?;
    Ok(previous
        .keys()
        .chain(current.keys())
        .filter(|path| previous.get(**path) != current.get(**path))
        .map(|path| (*path).to_owned())
        .collect())
}

fn validate_allowance_contract(allowance: &SplitAllowance) -> Result<()> {
    if allowance.approved_physical_lines == 0 && allowance.approved_production_lines == 0 {
        bail!("split allowance {:?} must approve nonzero overhead", allowance.id);
    }
    if allowance.approved_production_lines > allowance.approved_physical_lines {
        bail!("split allowance {:?} production overhead cannot exceed physical overhead", allowance.id);
    }
    if allowance.current_physical_lines > allowance.approved_physical_lines || allowance.current_production_lines > allowance.approved_production_lines {
        bail!("split allowance {:?} current overhead cannot exceed its approved overhead", allowance.id);
    }
    let current_is_zero = allowance.current_physical_lines == 0 && allowance.current_production_lines == 0;
    match allowance.status {
        SplitAllowanceStatus::Active if current_is_zero => {
            bail!("split allowance {:?} has no remaining overhead and must be resolved", allowance.id);
        }
        SplitAllowanceStatus::Resolved if !current_is_zero => {
            bail!("resolved split allowance {:?} must have zero remaining overhead", allowance.id);
        }
        SplitAllowanceStatus::Active | SplitAllowanceStatus::Resolved => {}
    }
    require_text(&allowance.id, "split allowance", "owner", &allowance.owner)?;
    require_text(&allowance.id, "split allowance", "recovery issue", &allowance.recovery_issue)?;
    require_text(&allowance.id, "split allowance", "pull request", &allowance.pull_request)?;
    require_text(&allowance.id, "split allowance", "rationale", &allowance.rationale)
}

fn validate_active_ownership(allowance: &SplitAllowance, hotspot: &Hotspot, current_paths: &BTreeMap<&str, &str>) -> Result<()> {
    if allowance.status == SplitAllowanceStatus::Resolved {
        return Ok(());
    }
    if hotspot
        .successors
        .iter()
        .any(|successor| current_paths.get(successor.as_str()) != Some(&hotspot.component.as_str()))
    {
        bail!("active split allowance {:?} cannot be transferred outside its hotspot component", allowance.id);
    }
    Ok(())
}

fn compare_existing_allowance(
    previous: &SplitAllowance,
    current: &SplitAllowance,
    previous_manifest: &StructureManifest,
    current_manifest: &StructureManifest,
    touched: &BTreeSet<String>,
) -> Result<()> {
    if !same_approval_identity(previous, current) {
        bail!("split allowance {:?} approval evidence is immutable", previous.id);
    }
    if previous.status == SplitAllowanceStatus::Resolved {
        if current != previous {
            bail!("resolved split allowance {:?} is immutable", previous.id);
        }
        return Ok(());
    }
    if current.current_physical_lines > previous.current_physical_lines || current.current_production_lines > previous.current_production_lines {
        bail!("split allowance {:?} remaining overhead cannot increase", current.id);
    }
    if successor_set_touched(previous, previous_manifest, current_manifest, touched)? {
        require_strict_reduction(previous, current, current_manifest.program_phase)?;
    }
    Ok(())
}

fn validate_new_allowance(allowance: &SplitAllowance, previous: &StructureManifest, current: &StructureManifest, evidence: &SplitPolicyEvidence<'_, '_, '_>) -> Result<()> {
    if allowance.status != SplitAllowanceStatus::Active
        || allowance.approved_physical_lines != allowance.current_physical_lines
        || allowance.approved_production_lines != allowance.current_production_lines
    {
        bail!("new split allowance {:?} must start active at its exact approved overhead", allowance.id);
    }
    if allowance.due_phase <= current.program_phase {
        bail!("new split allowance {:?} must have a future due phase", allowance.id);
    }
    let evolution = evidence
        .new_evolutions
        .iter()
        .find(|evolution| evolution.id == allowance.path_evolution)
        .with_context(|| format!("new split allowance {:?} must reference a split added in the same change", allowance.id))?;
    let hotspot = previous
        .hotspots
        .iter()
        .find(|hotspot| hotspot.id == allowance.hotspot)
        .with_context(|| format!("new split allowance {:?} references an unknown previous hotspot", allowance.id))?;
    if hotspot.status != HotspotStatus::Active {
        bail!("new split allowance {:?} requires an active baseline hotspot", allowance.id);
    }
    if hotspot_had_prior_split(previous, hotspot) {
        bail!("hotspot {:?} already used its one structural split allowance", hotspot.id);
    }
    if !evolution.sources.iter().all(|source| hotspot.successors.contains(source)) {
        bail!("new split allowance {:?} does not split its hotspot's current successor set", allowance.id);
    }
    validate_exact_growth(allowance, evolution, evidence.previous_files, evidence.current_files)?;
    validate_three_percent_cap(allowance, evolution, evidence.previous_files, evidence.current_files)
}

fn validate_exact_growth(allowance: &SplitAllowance, evolution: &PathEvolution, previous: &ObservedFiles<'_>, current: &ObservedFiles<'_>) -> Result<()> {
    let source = sum_counts(&evolution.sources, previous)?;
    let successor = sum_counts(&evolution.successors, current)?;
    let growth = LineCounts {
        physical: successor.physical.saturating_sub(source.physical),
        production: successor.production.saturating_sub(source.production),
    };
    if growth.physical != allowance.approved_physical_lines || growth.production != allowance.approved_production_lines {
        bail!("split allowance {:?} must equal the exact measured split overhead", allowance.id);
    }
    Ok(())
}

fn validate_three_percent_cap(allowance: &SplitAllowance, evolution: &PathEvolution, previous: &ObservedFiles<'_>, current: &ObservedFiles<'_>) -> Result<()> {
    let source_path = &evolution.sources[0];
    let source = previous.get(source_path.as_str()).context("split allowance source is missing")?;
    let remaining = current.get(source_path.as_str());
    let moved_physical = source
        .physical_lines
        .checked_sub(remaining.map_or(0, |file| file.physical_lines))
        .context("split allowance source grew instead of moving lines")?;
    let moved_production = source
        .production_lines
        .checked_sub(remaining.map_or(0, |file| file.production_lines))
        .context("split allowance source production grew instead of moving lines")?;
    let physical_cap = three_percent(moved_physical)?;
    let production_cap = three_percent(moved_production)?;
    if allowance.approved_physical_lines > physical_cap || allowance.approved_production_lines > production_cap {
        bail!(
            "split allowance {:?} exceeds 3% of moved lines: approved=({},{}) cap=({physical_cap},{production_cap})",
            allowance.id,
            allowance.approved_physical_lines,
            allowance.approved_production_lines
        );
    }
    Ok(())
}

fn three_percent(lines: usize) -> Result<usize> {
    lines.checked_mul(3).context("split allowance percentage overflow").map(|scaled| scaled / 100)
}

fn hotspot_had_prior_split(manifest: &StructureManifest, hotspot: &Hotspot) -> bool {
    let mut lineage = BTreeSet::from([hotspot.baseline_path.as_str()]);
    for evolution in &manifest.path_evolutions {
        if evolution.sources.iter().any(|source| lineage.contains(source.as_str())) {
            if evolution.kind == PathEvolutionKind::Split {
                return true;
            }
            for source in &evolution.sources {
                lineage.remove(source.as_str());
            }
            lineage.extend(evolution.successors.iter().map(String::as_str));
        }
    }
    false
}

fn successor_set_touched(allowance: &SplitAllowance, previous: &StructureManifest, current: &StructureManifest, touched: &BTreeSet<String>) -> Result<bool> {
    let previous_hotspot = previous
        .hotspots
        .iter()
        .find(|hotspot| hotspot.id == allowance.hotspot)
        .context("previous split allowance hotspot is missing")?;
    let current_hotspot = current
        .hotspots
        .iter()
        .find(|hotspot| hotspot.id == allowance.hotspot)
        .context("current split allowance hotspot is missing")?;
    Ok(previous_hotspot
        .successors
        .iter()
        .chain(&current_hotspot.successors)
        .any(|successor| touched.contains(successor)))
}

fn require_strict_reduction(previous: &SplitAllowance, current: &SplitAllowance, program_phase: u32) -> Result<()> {
    let physical_stale = previous.current_physical_lines > 0 && current.current_physical_lines >= previous.current_physical_lines;
    let production_stale = previous.current_production_lines > 0 && current.current_production_lines >= previous.current_production_lines;
    if physical_stale || production_stale {
        let timing = if program_phase >= current.due_phase { "overdue" } else { "active" };
        bail!(
            "{timing} split allowance {:?} successor touch must strictly reduce every outstanding overhead class",
            current.id
        );
    }
    Ok(())
}

fn same_approval_identity(previous: &SplitAllowance, current: &SplitAllowance) -> bool {
    previous.id == current.id
        && previous.hotspot == current.hotspot
        && previous.path_evolution == current.path_evolution
        && previous.approved_physical_lines == current.approved_physical_lines
        && previous.approved_production_lines == current.approved_production_lines
        && previous.owner == current.owner
        && previous.recovery_issue == current.recovery_issue
        && previous.pull_request == current.pull_request
        && previous.rationale == current.rationale
        && previous.due_phase == current.due_phase
}

fn hotspot_index(manifest: &StructureManifest) -> BTreeMap<&str, &Hotspot> {
    manifest.hotspots.iter().map(|hotspot| (hotspot.id.as_str(), hotspot)).collect()
}
