use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};

use super::exceptions::{file_kind, ordinary_file_limit};
use super::measure::{ObservedFiles, compare_file_sets, inventory_index, sum_physical_lines, sum_production_lines};
use super::model::{Hotspot, HotspotKind, HotspotStatus, StructureManifest};
use crate::structure::classify::Inventory;

impl StructureManifest {
    pub fn compare_current(&self, inventory: &Inventory) -> Result<()> {
        let observed = inventory_index(inventory)?;
        let mapped = self.current_path_map()?;
        compare_file_sets("current", mapped.keys(), observed.keys())?;
        self.compare_component_counts(&observed)?;
        self.compare_file_exceptions(&observed)?;
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
            let transfer_adjusted_baseline = self.transfer_adjusted_baseline(&component.id, adjusted_baseline)?;
            if component.production_ceiling > transfer_adjusted_baseline {
                bail!(
                    "component {:?} production ceiling {} exceeds its verified transfer-adjusted baseline {transfer_adjusted_baseline}",
                    component.id,
                    component.production_ceiling
                );
            }
        }
        self.compare_baseline_hotspots(&observed)
    }

    fn transfer_adjusted_baseline(&self, component: &str, adjusted_baseline: usize) -> Result<usize> {
        let outgoing = transfer_total(
            self.component_transfers
                .iter()
                .filter(|transfer| transfer.source_component == component)
                .map(|transfer| transfer.production_lines),
        )?;
        let incoming = transfer_total(
            self.component_transfers
                .iter()
                .filter(|transfer| transfer.destination_component == component)
                .map(|transfer| transfer.production_lines),
        )?;
        adjusted_baseline
            .checked_add(incoming)
            .context("component transfer-adjusted baseline overflow")?
            .checked_sub(outgoing)
            .with_context(|| format!("component {component:?} transfers exceed its incoming-adjusted baseline"))
    }

    fn compare_component_counts(&self, observed: &ObservedFiles<'_>) -> Result<()> {
        for component in &self.components {
            let production = sum_production_lines(&component.paths, observed)?;
            let allowance = self.component_split_production_allowance(&component.id)?;
            let maximum = component.production_ceiling.checked_add(allowance).context("component split allowance ceiling overflow")?;
            if production > maximum {
                bail!(
                    "component {:?} production growth rejected: canonical ceiling={}, split allowance={allowance}, observed={production}",
                    component.id,
                    component.production_ceiling,
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

    fn compare_hotspot_counts(&self, observed: &ObservedFiles<'_>) -> Result<()> {
        for hotspot in &self.hotspots {
            self.compare_current_hotspot(hotspot, observed)?;
        }
        Ok(())
    }

    fn compare_file_limits(&self, observed: &ObservedFiles<'_>) -> Result<()> {
        let active_successors: BTreeSet<_> = self
            .hotspots
            .iter()
            .filter(|hotspot| hotspot.status == HotspotStatus::Active)
            .flat_map(|hotspot| hotspot.successors.iter().map(String::as_str))
            .collect();
        for file in observed.values() {
            let limit = self.applicable_file_limit(file);
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

    fn compare_baseline_hotspots(&self, observed: &ObservedFiles<'_>) -> Result<()> {
        let expected: BTreeSet<_> = observed
            .values()
            .filter(|file| file.physical_lines > ordinary_file_limit(file))
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
    fn compare_current_hotspot(&self, hotspot: &Hotspot, observed: &ObservedFiles<'_>) -> Result<()> {
        let physical = sum_physical_lines(&hotspot.successors, observed)?;
        let production = sum_production_lines(&hotspot.successors, observed)?;
        let allowance = self.split_allowance_counts(&hotspot.id);
        compare_ratchet("physical", &hotspot.id, hotspot.physical_ceiling, allowance.physical, physical)?;
        compare_ratchet("production", &hotspot.id, hotspot.production_ceiling, allowance.production, production)?;
        match hotspot.status {
            HotspotStatus::Active => {
                validate_hotspot_kind(hotspot, production)?;
                if hotspot
                    .successors
                    .iter()
                    .all(|path| observed.get(path.as_str()).is_some_and(|file| file.physical_lines <= self.applicable_file_limit(file)))
                {
                    bail!("hotspot {:?} is below every successor file limit and must be marked resolved", hotspot.id);
                }
            }
            HotspotStatus::Resolved => self.validate_resolved_successors(hotspot, observed)?,
        }
        Ok(())
    }

    fn validate_resolved_successors(&self, hotspot: &Hotspot, observed: &ObservedFiles<'_>) -> Result<()> {
        for successor in &hotspot.successors {
            if let Some(file) = observed.get(successor.as_str())
                && file.physical_lines > self.applicable_file_limit(file)
            {
                bail!("resolved hotspot {:?} successor {successor:?} exceeds its applicable file limit", hotspot.id);
            }
        }
        Ok(())
    }
}

fn transfer_total(mut amounts: impl Iterator<Item = usize>) -> Result<usize> {
    amounts.try_fold(0usize, |total, amount| total.checked_add(amount).context("component transfer total overflow"))
}

fn compare_ratchet(kind: &str, id: &str, ceiling: usize, allowance: usize, observed: usize) -> Result<()> {
    let maximum = ceiling.checked_add(allowance).context("hotspot split allowance ceiling overflow")?;
    if observed > maximum {
        bail!("hotspot {id:?} {kind} growth rejected: canonical ceiling={ceiling}, split allowance={allowance}, observed={observed}");
    }
    if observed < ceiling {
        bail!("hotspot {id:?} {kind} ceiling must ratchet from {ceiling} to {observed} in this change");
    }
    if observed < maximum {
        bail!("hotspot {id:?} {kind} split allowance must ratchet from {allowance} to {}", observed - ceiling);
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
