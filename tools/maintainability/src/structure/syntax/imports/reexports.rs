use std::collections::BTreeSet;

use crate::scan::syntax_fingerprint;

use super::super::ProductionCfgContext;
use super::PublicReexportEvidence;
use super::concrete::{ConcreteStoreSignatureSite, ConcreteStoreSignatureSites};

pub(super) struct PendingPublicReexport {
    pub(super) evidence: PublicReexportEvidence,
    pub(super) cfg: ProductionCfgContext,
}

#[derive(Clone, Debug)]
pub(super) struct UseResolution {
    pub(super) exported_path: Vec<String>,
    pub(super) target_path: Vec<String>,
    pub(super) fingerprint: String,
    pub(super) cfg: ProductionCfgContext,
}

type ResolvedUseTargets = Vec<(Vec<String>, Vec<String>, ProductionCfgContext)>;

struct AliasResolver<'a> {
    resolutions: &'a [UseResolution],
    targets: &'a mut ResolvedUseTargets,
}

struct UseRewrite {
    target: Vec<String>,
    fingerprint: String,
    cfg: ProductionCfgContext,
    is_glob: bool,
}

pub(super) fn resolve_public_reexport_aliases(reexports: Vec<PendingPublicReexport>, resolutions: &[UseResolution]) -> Vec<PublicReexportEvidence> {
    let mut resolved = Vec::new();
    for pending in reexports {
        let evidence = pending.evidence;
        for (target_path, alias_fingerprints, cfg) in resolve_use_aliases(evidence.target_path.clone(), &pending.cfg, resolutions) {
            let fingerprint = if alias_fingerprints.is_empty() && target_path == evidence.target_path {
                evidence.fingerprint.clone()
            } else {
                syntax_fingerprint(&format!(
                    "{}\0resolved-target:{}\0aliases:{}",
                    evidence.fingerprint,
                    target_path.join("::"),
                    alias_fingerprints.join("\0")
                ))
            };
            resolved.push(PublicReexportEvidence {
                exported_path: evidence.exported_path.clone(),
                target_path,
                fingerprint,
                cfg,
            });
        }
    }
    resolved
}

pub(super) fn resolve_impl_signature_aliases(sites: &mut ConcreteStoreSignatureSites, resolutions: &[UseResolution]) {
    resolve_impl_signature_site_aliases(&mut sites.sqlite_store, resolutions);
    resolve_impl_signature_site_aliases(&mut sites.postgres_store, resolutions);
}

fn resolve_impl_signature_site_aliases(sites: &mut Vec<ConcreteStoreSignatureSite>, resolutions: &[UseResolution]) {
    let mut resolved = Vec::new();
    for site in std::mem::take(sites) {
        if !site.impl_self_type || site.item_path.is_empty() {
            resolved.push(site);
            continue;
        }
        for (item_path, alias_fingerprints, cfg) in resolve_use_aliases(site.item_path.clone(), &site.cfg, resolutions) {
            let fingerprint = if alias_fingerprints.is_empty() && item_path == site.item_path {
                site.fingerprint.clone()
            } else {
                syntax_fingerprint(&format!(
                    "{}\0resolved-impl-self-type:{}\0aliases:{}",
                    site.fingerprint,
                    item_path.join("::"),
                    alias_fingerprints.join("\0")
                ))
            };
            resolved.push(ConcreteStoreSignatureSite {
                fingerprint,
                item_path,
                cfg,
                impl_self_type: true,
            });
        }
    }
    *sites = resolved;
}

fn resolve_use_aliases(path: Vec<String>, cfg: &ProductionCfgContext, resolutions: &[UseResolution]) -> ResolvedUseTargets {
    let mut targets = Vec::new();
    AliasResolver {
        resolutions,
        targets: &mut targets,
    }
    .resolve(path, cfg, &mut BTreeSet::new(), &mut Vec::new());
    targets
}

impl AliasResolver<'_> {
    fn resolve(&mut self, path: Vec<String>, cfg: &ProductionCfgContext, visited: &mut BTreeSet<Vec<String>>, alias_fingerprints: &mut Vec<String>) {
        if !visited.insert(path.clone()) {
            self.targets.push((path, alias_fingerprints.clone(), cfg.clone()));
            return;
        }
        let rewritten = self
            .resolutions
            .iter()
            .filter_map(|resolution| {
                let compatible_cfg = cfg.conjoin(&resolution.cfg)?;
                rewrite_use_target(&path, resolution).map(|target| UseRewrite {
                    target,
                    fingerprint: resolution.fingerprint.clone(),
                    cfg: compatible_cfg,
                    is_glob: resolution.exported_path.last().is_some_and(|segment| segment == "*"),
                })
            })
            .filter(|rewrite| rewrite.target != path)
            .collect::<Vec<_>>();
        let explicit_cfgs = rewritten.iter().filter(|rewrite| !rewrite.is_glob).map(|rewrite| rewrite.cfg.clone()).collect::<Vec<_>>();
        if rewritten.is_empty() {
            self.targets.push((path, alias_fingerprints.clone(), cfg.clone()));
        } else {
            for rewrite in rewritten.into_iter().filter_map(|rewrite| apply_explicit_precedence(rewrite, &explicit_cfgs)) {
                alias_fingerprints.push(rewrite.fingerprint);
                self.resolve(rewrite.target, &rewrite.cfg, &mut visited.clone(), alias_fingerprints);
                alias_fingerprints.pop();
            }
        }
    }
}

fn apply_explicit_precedence(mut rewrite: UseRewrite, explicit_cfgs: &[ProductionCfgContext]) -> Option<UseRewrite> {
    if rewrite.is_glob {
        rewrite.cfg = explicit_cfgs.iter().try_fold(rewrite.cfg, |remaining, explicit_cfg| remaining.excluding(explicit_cfg))?;
    }
    Some(rewrite)
}

fn rewrite_use_target(path: &[String], resolution: &UseResolution) -> Option<Vec<String>> {
    let exported_glob = resolution.exported_path.last().is_some_and(|segment| segment == "*");
    let exported_prefix = resolution.exported_path.strip_suffix(&["*".to_owned()]).unwrap_or(&resolution.exported_path);
    if !path.starts_with(exported_prefix) || exported_glob && path.len() == exported_prefix.len() {
        return None;
    }
    let target_prefix = resolution.target_path.strip_suffix(&["*".to_owned()]).unwrap_or(&resolution.target_path);
    let mut resolved = target_prefix.to_vec();
    resolved.extend_from_slice(&path[exported_prefix.len()..]);
    Some(resolved)
}
