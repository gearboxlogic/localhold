use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};

use super::external_content_fingerprint;

type ExternalTargets = BTreeMap<String, BTreeMap<String, String>>;
type TargetSources = BTreeMap<(String, String), BTreeSet<String>>;

pub(in crate::structure::suppression) fn external_target_maps(relations: &BTreeMap<(String, String), String>, syntax: &BTreeMap<String, syn::File>) -> Result<ExternalTargets> {
    let mut adjacency: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for ((parent, _), child) in relations {
        adjacency.entry(parent).or_default().insert(child);
    }
    let mut target_sources = TargetSources::new();
    for ((parent, item), child) in relations {
        let reachable = reachable_sources(child, &adjacency);
        for target in module_target_ancestors(item) {
            target_sources.entry((parent.clone(), target)).or_default().extend(reachable.iter().cloned());
        }
        target_sources.entry((parent.clone(), "<module>".to_owned())).or_default().extend(reachable);
    }
    let mut targets = ExternalTargets::new();
    for ((parent, item), sources) in target_sources {
        let sources = sources
            .iter()
            .map(|path| {
                syntax
                    .get(path)
                    .map(|parsed| (path.as_str(), parsed))
                    .with_context(|| format!("external module fingerprint source is missing {path:?}"))
            })
            .collect::<Result<Vec<_>>>()?;
        targets.entry(parent).or_default().insert(item, external_content_fingerprint(sources));
    }
    Ok(targets)
}

fn reachable_sources(root: &str, adjacency: &BTreeMap<&str, BTreeSet<&str>>) -> BTreeSet<String> {
    let mut reachable = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(path) = pending.pop() {
        if !reachable.insert(path.to_owned()) {
            continue;
        }
        if let Some(children) = adjacency.get(path) {
            pending.extend(children);
        }
    }
    reachable
}

fn module_target_ancestors(item: &str) -> Vec<String> {
    let mut current = String::new();
    item.split("::")
        .map(|segment| {
            if !current.is_empty() {
                current.push_str("::");
            }
            current.push_str(segment);
            current.clone()
        })
        .collect()
}
