use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use self::model::PathEvolution;
pub use self::model::StructureManifest;

mod comparison;
mod evolution;
mod exceptions;
mod measure;
mod model;
mod revision;
mod split_allowances;
mod validate;

impl StructureManifest {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read structure manifest {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes).with_context(|| format!("parse structure manifest {}", path.display()))?;
        manifest.validate_current()?;
        Ok(manifest)
    }

    pub(super) fn current_component_paths(&self) -> Result<BTreeMap<&str, &str>> {
        measure::component_path_map(&self.components, |component| &component.paths)
    }

    pub(super) fn baseline_component_paths(&self) -> Result<BTreeMap<&str, &str>> {
        measure::component_path_map(&self.components, |component| &component.baseline_paths)
    }

    pub(super) fn canonical_current_paths(&self) -> Result<BTreeMap<String, String>> {
        let mut lineage = self
            .baseline_component_paths()?
            .into_keys()
            .map(|path| (path.to_owned(), path.to_owned()))
            .collect::<BTreeMap<_, _>>();
        for evolution in &self.path_evolutions {
            evolve_path_lineage(&mut lineage, evolution)?;
        }

        let current = self.current_component_paths()?;
        let current_paths = current.keys().copied().collect::<std::collections::BTreeSet<_>>();
        lineage.retain(|path, _| current_paths.contains(path.as_str()));
        for path in &current_paths {
            lineage.entry((*path).to_owned()).or_insert_with(|| (*path).to_owned());
        }
        let lineage_paths = lineage.keys().map(String::as_str).collect::<std::collections::BTreeSet<_>>();
        if lineage_paths != current_paths {
            bail!("current structural paths do not match their governed baseline path lineage");
        }
        Ok(lineage)
    }
}

fn evolve_path_lineage(lineage: &mut BTreeMap<String, String>, evolution: &PathEvolution) -> Result<()> {
    let [source] = evolution.sources.as_slice() else {
        bail!("path evolution {:?} must have exactly one source", evolution.id);
    };
    let canonical = lineage
        .remove(source)
        .with_context(|| format!("path evolution {:?} source {source:?} has no active baseline lineage", evolution.id))?;
    for successor in &evolution.successors {
        if let Some(existing) = lineage.insert(successor.clone(), canonical.clone()) {
            bail!("path evolution {:?} successor {successor:?} collides with baseline lineage {existing:?}", evolution.id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
