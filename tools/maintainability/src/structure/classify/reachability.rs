use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::ProductionCfgContext;

#[derive(Debug)]
pub(super) struct ModuleEdge {
    pub(super) source: String,
    pub(super) target: String,
    pub(super) test_only: bool,
    pub(super) production_context: Option<ProductionCfgContext>,
}

pub(super) fn production_contexts(edges: &[ModuleEdge], roots: &BTreeSet<String>) -> Result<BTreeMap<String, ProductionCfgContext>> {
    let mut paths = BTreeMap::<String, BTreeMap<String, ProductionCfgContext>>::new();
    for root in roots {
        collect_context_paths(root, &ProductionCfgContext::default(), edges, &mut BTreeSet::new(), &mut paths)?;
    }
    paths
        .into_iter()
        .map(|(path, contexts)| {
            let context = ProductionCfgContext::disjunction(contexts.into_values()).context("production source has no satisfiable cfg path")?;
            Ok((path, context))
        })
        .collect()
}

fn collect_context_paths(
    source: &str,
    inherited: &ProductionCfgContext,
    edges: &[ModuleEdge],
    active: &mut BTreeSet<String>,
    paths: &mut BTreeMap<String, BTreeMap<String, ProductionCfgContext>>,
) -> Result<()> {
    if !active.insert(source.to_owned()) {
        bail!("production module graph contains a cycle through {source:?}");
    }
    let identity = inherited.identity();
    if paths.entry(source.to_owned()).or_default().insert(identity, inherited.clone()).is_none() {
        for edge in edges.iter().filter(|edge| !edge.test_only && edge.source == source) {
            let local = edge.production_context.as_ref().context("production module edge has no cfg context")?;
            if let Some(context) = inherited.conjoin(local) {
                collect_context_paths(&edge.target, &context, edges, active, paths)?;
            }
        }
    }
    active.remove(source);
    Ok(())
}

pub(super) fn production_reachable_from(edges: &[ModuleEdge], roots: &BTreeSet<String>) -> BTreeSet<String> {
    let mut reachable = roots.clone();
    loop {
        let mut changed = false;
        for edge in edges {
            if !edge.test_only && reachable.contains(&edge.source) {
                changed |= reachable.insert(edge.target.clone());
            }
        }
        if !changed {
            return reachable;
        }
    }
}

pub(super) fn propagate_reachability(edges: &[ModuleEdge], production: &mut BTreeSet<String>, test: &mut BTreeSet<String>) {
    loop {
        let mut changed = false;
        for edge in edges {
            changed |= match (production.contains(&edge.source), edge.test_only) {
                (true, true) => test.insert(edge.target.clone()),
                (true, false) => production.insert(edge.target.clone()),
                (false, _) => false,
            };
            if test.contains(&edge.source) {
                changed |= test.insert(edge.target.clone());
            }
        }
        if !changed {
            break;
        }
    }
}
