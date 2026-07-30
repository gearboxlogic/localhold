use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use super::external_content_fingerprint;

type ExternalTargets = BTreeMap<String, BTreeMap<String, String>>;
type LogicalSources = BTreeMap<String, String>;
type ModuleAdjacency<'a> = BTreeMap<&'a str, Vec<(&'a str, &'a str)>>;
type TargetSources = BTreeMap<(String, String), LogicalSources>;

pub(in crate::structure::suppression) fn external_target_maps(relations: &BTreeMap<(String, String), String>, syntax: &BTreeMap<String, syn::File>) -> Result<ExternalTargets> {
    let mut adjacency = ModuleAdjacency::new();
    for ((parent, item), child) in relations {
        adjacency.entry(parent).or_default().push((item, child));
    }
    let mut target_sources = TargetSources::new();
    for ((parent, item), child) in relations {
        let reachable = reachable_sources(item, child, &adjacency)?;
        for target in module_target_ancestors(item) {
            merge_sources(target_sources.entry((parent.clone(), target)).or_default(), &reachable)?;
        }
        merge_sources(target_sources.entry((parent.clone(), "<module>".to_owned())).or_default(), &reachable)?;
    }
    let mut targets = ExternalTargets::new();
    for ((parent, item), sources) in target_sources {
        let sources = sources
            .iter()
            .map(|(logical, path)| {
                syntax
                    .get(path)
                    .map(|parsed| (logical.as_str(), parsed))
                    .with_context(|| format!("external module fingerprint source is missing {path:?}"))
            })
            .collect::<Result<Vec<_>>>()?;
        targets.entry(parent).or_default().insert(item, external_content_fingerprint(sources));
    }
    Ok(targets)
}

fn reachable_sources(root_item: &str, root_path: &str, adjacency: &ModuleAdjacency<'_>) -> Result<LogicalSources> {
    let mut reachable = LogicalSources::new();
    let mut pending = vec![(root_item.to_owned(), root_path)];
    while let Some((logical, path)) = pending.pop() {
        if !insert_logical_source(&mut reachable, logical.clone(), path)? {
            continue;
        }
        if let Some(children) = adjacency.get(path) {
            pending.extend(children.iter().map(|(item, child)| (format!("{logical}::{item}"), *child)));
        }
    }
    Ok(reachable)
}

fn merge_sources(target: &mut LogicalSources, sources: &LogicalSources) -> Result<()> {
    for (logical, path) in sources {
        insert_logical_source(target, logical.clone(), path)?;
    }
    Ok(())
}

fn insert_logical_source(sources: &mut LogicalSources, logical: String, path: &str) -> Result<bool> {
    if let Some(existing) = sources.get(&logical) {
        if existing != path {
            bail!("logical external module {logical:?} resolves to multiple source files");
        }
        return Ok(false);
    }
    sources.insert(logical, path.to_owned());
    Ok(true)
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
