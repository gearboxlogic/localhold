use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use super::{VisibilityException, VisibilityKind, VisibilityPolicy};
use crate::structure::classify::Inventory;

type SubtreeDeltas<'a> = BTreeMap<(&'a str, &'a str), usize>;

impl VisibilityPolicy {
    pub fn compare_exception_scopes(&self, current: &Inventory, current_paths: &BTreeMap<&str, &str>, baseline: &Inventory, baseline_paths: &BTreeMap<&str, &str>) -> Result<()> {
        let subtree_deltas = exception_subtree_deltas(&self.exceptions)?;
        let mut subtrees = BTreeMap::<&str, Vec<&str>>::new();
        for &(component, subtree) in subtree_deltas.keys() {
            subtrees.entry(component).or_default().push(subtree);
        }
        for (component, approved) in subtrees {
            compare_subtree_ceilings((current, current_paths), (baseline, baseline_paths), component, &approved, &subtree_deltas)?;
            let current_outside = count_outside_subtrees(current, current_paths, component, &approved)?;
            let baseline_outside = count_outside_subtrees(baseline, baseline_paths, component, &approved)?;
            if current_outside > baseline_outside {
                bail!(
                    "pub(super) visibility growth for component {component:?} escaped its approved component subtree: baseline_outside={baseline_outside}, current_outside={current_outside}"
                );
            }
            if !subtrees_cover_component_path(current_paths, baseline_paths, component, &approved) {
                bail!("pub(super) visibility exception for component {component:?} does not select a current or baseline component path");
            }
        }
        Ok(())
    }
}

pub(super) fn validate_non_overlapping_subtrees(exceptions: &[VisibilityException]) -> Result<()> {
    let deltas = exception_subtree_deltas(exceptions)?;
    let entries = deltas.keys().copied().collect::<Vec<_>>();
    for (index, (component, subtree)) in entries.iter().enumerate() {
        for (other_component, other_subtree) in &entries[index + 1..] {
            if component == other_component && (path_in_subtree(subtree, other_subtree) || path_in_subtree(other_subtree, subtree)) {
                bail!("pub(super) visibility exception subtrees for component {component:?} must not overlap: {subtree:?} and {other_subtree:?}");
            }
        }
    }
    Ok(())
}

fn compare_subtree_ceilings(
    current: (&Inventory, &BTreeMap<&str, &str>),
    baseline: (&Inventory, &BTreeMap<&str, &str>),
    component: &str,
    subtrees: &[&str],
    deltas: &SubtreeDeltas<'_>,
) -> Result<()> {
    for subtree in subtrees {
        let current_inside = count_in_subtree(current.0, current.1, component, subtree)?;
        let baseline_inside = count_in_subtree(baseline.0, baseline.1, component, subtree)?;
        let delta = deltas[&(component, *subtree)];
        let ceiling = baseline_inside.checked_add(delta).context("pub(super) subtree visibility count overflow")?;
        if current_inside > ceiling {
            bail!(
                "pub(super) visibility growth for component {component:?} exceeded its reviewed subtree ceiling for {subtree:?}: baseline_inside={baseline_inside}, reviewed_delta={delta}, current_inside={current_inside}"
            );
        }
    }
    Ok(())
}

fn exception_subtree_deltas(exceptions: &[VisibilityException]) -> Result<SubtreeDeltas<'_>> {
    let mut totals = BTreeMap::new();
    for exception in exceptions.iter().filter(|exception| exception.kind == VisibilityKind::PubSuper) {
        let subtree = exception.subtree.as_deref().context("validated pub(super) exception is missing its subtree")?;
        let total = totals.entry((exception.component.as_str(), subtree)).or_insert(0_usize);
        *total = total.checked_add(exception.delta).context("visibility subtree exception delta overflow")?;
    }
    Ok(totals)
}

fn count_outside_subtrees(inventory: &Inventory, component_paths: &BTreeMap<&str, &str>, component: &str, subtrees: &[&str]) -> Result<usize> {
    count_matching(inventory, component_paths, component, |path| !subtrees.iter().any(|subtree| path_in_subtree(path, subtree)))
}

fn count_in_subtree(inventory: &Inventory, component_paths: &BTreeMap<&str, &str>, component: &str, subtree: &str) -> Result<usize> {
    count_matching(inventory, component_paths, component, |path| path_in_subtree(path, subtree))
}

fn count_matching(inventory: &Inventory, component_paths: &BTreeMap<&str, &str>, component: &str, include: impl Fn(&str) -> bool) -> Result<usize> {
    inventory
        .files
        .iter()
        .filter(|file| component_paths.get(file.path.as_str()).copied() == Some(component))
        .filter(|file| include(&file.path))
        .try_fold(0_usize, |total, file| {
            total.checked_add(file.production_visibilities.pub_super).context("pub(super) visibility count overflow")
        })
}

fn subtrees_cover_component_path(current: &BTreeMap<&str, &str>, baseline: &BTreeMap<&str, &str>, component: &str, subtrees: &[&str]) -> bool {
    current
        .iter()
        .chain(baseline.iter())
        .any(|(path, mapped)| *mapped == component && subtrees.iter().any(|subtree| path_in_subtree(path, subtree)))
}

fn path_in_subtree(path: &str, subtree: &str) -> bool {
    path == subtree || path.strip_prefix(subtree).is_some_and(|suffix| suffix.starts_with('/'))
}
