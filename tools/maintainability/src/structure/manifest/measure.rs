use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::model::LogicalComponent;
use super::validate::validate_relative_rust_path;
use crate::structure::classify::{FileMeasurement, Inventory};

pub(super) type ObservedFiles<'a> = BTreeMap<&'a str, &'a FileMeasurement>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LineCounts {
    pub(super) physical: usize,
    pub(super) production: usize,
}

pub(super) fn inventory_index(inventory: &Inventory) -> Result<ObservedFiles<'_>> {
    let mut files = BTreeMap::new();
    for file in &inventory.files {
        validate_relative_rust_path(&file.path)?;
        if file.production_lines.checked_add(file.test_lines).context("structural inventory line count overflow")? != file.physical_lines {
            bail!("structural inventory counts do not sum for {}", file.path);
        }
        if files.insert(file.path.as_str(), file).is_some() {
            bail!("structural inventory repeats {}", file.path);
        }
    }
    Ok(files)
}

pub(super) fn component_path_map<'a>(components: &'a [LogicalComponent], select: impl Fn(&'a LogicalComponent) -> &'a [String]) -> Result<BTreeMap<&'a str, &'a str>> {
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

pub(super) fn compare_file_sets<'a>(label: &str, mapped: impl Iterator<Item = &'a &'a str>, observed: impl Iterator<Item = &'a &'a str>) -> Result<()> {
    let mapped: BTreeSet<_> = mapped.copied().collect();
    let observed: BTreeSet<_> = observed.copied().collect();
    if mapped != observed {
        let missing: Vec<_> = mapped.difference(&observed).copied().collect();
        let unmapped: Vec<_> = observed.difference(&mapped).copied().collect();
        bail!("{label} structural file map mismatch: missing={missing:?}, unmapped={unmapped:?}");
    }
    Ok(())
}

pub(super) fn compare_key_sets<'a>(label: &str, current: impl Iterator<Item = &'a &'a str>, previous: impl Iterator<Item = &'a &'a str>) -> Result<()> {
    let current: BTreeSet<_> = current.copied().collect();
    let previous: BTreeSet<_> = previous.copied().collect();
    if current != previous {
        bail!("{label} baseline IDs are immutable: current={current:?}, previous={previous:?}");
    }
    Ok(())
}

pub(super) fn sum_counts(paths: &[String], observed: &ObservedFiles<'_>) -> Result<LineCounts> {
    paths.iter().try_fold(LineCounts { physical: 0, production: 0 }, |total, path| {
        let file = observed.get(path.as_str()).with_context(|| format!("structural source {path:?} is missing"))?;
        Ok(LineCounts {
            physical: total.physical.checked_add(file.physical_lines).context("physical line count overflow")?,
            production: total.production.checked_add(file.production_lines).context("production line count overflow")?,
        })
    })
}

pub(super) fn sum_production_lines(paths: &[String], observed: &ObservedFiles<'_>) -> Result<usize> {
    Ok(sum_counts(paths, observed)?.production)
}

pub(super) fn sum_physical_lines(paths: &[String], observed: &ObservedFiles<'_>) -> Result<usize> {
    Ok(sum_counts(paths, observed)?.physical)
}
