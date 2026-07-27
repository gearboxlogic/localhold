use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};

use super::measure::{ObservedFiles, compare_file_sets, inventory_index, sum_physical_lines, sum_production_lines};
use super::model::{Hotspot, HotspotKind, HotspotStatus, PRODUCTION_FILE_LIMIT, StructureManifest, TEST_FILE_LIMIT};
use crate::structure::classify::{FileMeasurement, Inventory};

impl StructureManifest {
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

    fn compare_component_counts(&self, observed: &ObservedFiles<'_>) -> Result<()> {
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

    fn compare_hotspot_counts(&self, observed: &ObservedFiles<'_>) -> Result<()> {
        for hotspot in &self.hotspots {
            compare_current_hotspot(hotspot, observed)?;
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

    fn compare_baseline_hotspots(&self, observed: &ObservedFiles<'_>) -> Result<()> {
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
}

fn compare_current_hotspot(hotspot: &Hotspot, observed: &ObservedFiles<'_>) -> Result<()> {
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

fn validate_resolved_successors(hotspot: &Hotspot, observed: &ObservedFiles<'_>) -> Result<()> {
    for successor in &hotspot.successors {
        if let Some(file) = observed.get(successor.as_str())
            && file.physical_lines > applicable_limit(file)
        {
            bail!("resolved hotspot {:?} successor {successor:?} exceeds its ordinary file limit", hotspot.id);
        }
    }
    Ok(())
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
