use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::{ProductionAncestorPath, ProductionCfgContext};

#[derive(Debug)]
pub(super) struct ModuleEdge {
    pub(super) source: String,
    pub(super) target: String,
    pub(super) test_only: bool,
    pub(super) production_context: Option<ProductionCfgContext>,
    pub(super) declaration_ancestors: Vec<String>,
    pub(super) modules_exposed: bool,
}

#[derive(Clone)]
pub(super) struct ProductionSourceContext {
    pub(super) cfg: ProductionCfgContext,
    pub(super) declaration_ancestors: Vec<ProductionAncestorPath>,
    pub(super) module_exposure_cfg: Option<ProductionCfgContext>,
}

#[derive(Clone)]
struct ProductionPathContext {
    cfg: ProductionCfgContext,
    declaration_ancestors: Vec<String>,
    modules_exposed: bool,
}

struct ProductionPathEdge {
    target: String,
    cfg: ProductionCfgContext,
    declaration_ancestors: Vec<String>,
    modules_exposed: bool,
}

struct ContextPathCollector<'a> {
    edges: &'a [ModuleEdge],
    active: BTreeSet<String>,
    paths: BTreeMap<String, BTreeMap<String, ProductionPathContext>>,
}

pub(super) fn production_contexts(edges: &[ModuleEdge], roots: &BTreeSet<String>) -> Result<BTreeMap<String, ProductionSourceContext>> {
    let mut collector = ContextPathCollector {
        edges,
        active: BTreeSet::new(),
        paths: BTreeMap::new(),
    };
    for root in roots {
        collector.collect(
            root,
            &ProductionPathContext {
                cfg: ProductionCfgContext::default(),
                declaration_ancestors: Vec::new(),
                modules_exposed: true,
            },
        )?;
    }
    collector
        .paths
        .into_iter()
        .map(|(path, contexts)| {
            let cfg = ProductionCfgContext::disjunction(contexts.values().map(|context| context.cfg.clone())).context("production source has no satisfiable cfg path")?;
            let declaration_ancestors = contexts
                .values()
                .filter(|context| !context.declaration_ancestors.is_empty())
                .map(|context| ProductionAncestorPath {
                    cfg: context.cfg.clone(),
                    ancestors: context.declaration_ancestors.clone(),
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let module_exposure_cfg = ProductionCfgContext::disjunction(contexts.values().filter(|context| context.modules_exposed).map(|context| context.cfg.clone()));
            Ok((
                path,
                ProductionSourceContext {
                    cfg,
                    declaration_ancestors,
                    module_exposure_cfg,
                },
            ))
        })
        .collect()
}

impl ContextPathCollector<'_> {
    fn collect(&mut self, source: &str, inherited: &ProductionPathContext) -> Result<()> {
        if !self.active.insert(source.to_owned()) {
            bail!("production module graph contains a cycle through {source:?}");
        }
        let identity = format!(
            "{}\0{}\0modules-exposed:{}",
            inherited.cfg.identity(),
            inherited.declaration_ancestors.join("\0"),
            inherited.modules_exposed
        );
        if self.paths.entry(source.to_owned()).or_default().insert(identity, inherited.clone()).is_none() {
            let outgoing = self
                .edges
                .iter()
                .filter(|edge| !edge.test_only && edge.source == source)
                .map(|edge| {
                    Ok(ProductionPathEdge {
                        target: edge.target.clone(),
                        cfg: edge.production_context.clone().context("production module edge has no cfg context")?,
                        declaration_ancestors: edge.declaration_ancestors.clone(),
                        modules_exposed: edge.modules_exposed,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            for edge in outgoing {
                self.descend(inherited, edge)?;
            }
        }
        self.active.remove(source);
        Ok(())
    }

    fn descend(&mut self, inherited: &ProductionPathContext, edge: ProductionPathEdge) -> Result<()> {
        let Some(cfg) = inherited.cfg.conjoin(&edge.cfg) else {
            return Ok(());
        };
        let mut declaration_ancestors = inherited.declaration_ancestors.clone();
        declaration_ancestors.extend(edge.declaration_ancestors);
        self.collect(
            &edge.target,
            &ProductionPathContext {
                cfg,
                declaration_ancestors,
                modules_exposed: inherited.modules_exposed && edge.modules_exposed,
            },
        )
    }
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
