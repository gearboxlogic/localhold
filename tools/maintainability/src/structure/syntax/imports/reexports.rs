use std::collections::BTreeSet;

use crate::scan::syntax_fingerprint;
use anyhow::Result;
use syn::{ItemType, Type};

use super::super::{ProductionCfgContext, normalized_ident, visibility_is_exposed};
use super::PublicReexportEvidence;
use super::concrete::{ConcreteStoreBindingSite, ConcreteStoreBindingSites, ConcreteStoreSignatureSite, ConcreteStoreSignatureSites};
use super::resolution::resolve_path;

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

pub(super) struct PublicTypeAliasContext<'a> {
    pub(super) module: &'a [String],
    pub(super) cfg: &'a ProductionCfgContext,
    pub(super) direct_exposure_cfg: Option<ProductionCfgContext>,
    pub(super) ancestors: &'a str,
    pub(super) rust_2015_absolute_paths: bool,
}

pub(super) fn public_type_alias(item: &ItemType, context: PublicTypeAliasContext<'_>) -> Result<Option<PendingPublicReexport>> {
    if !visibility_is_exposed(&item.vis) {
        return Ok(None);
    }
    let Type::Path(alias_target) = item.ty.as_ref() else {
        return Ok(None);
    };
    if alias_target.qself.is_some() || alias_target.path.leading_colon.is_some() && !context.rust_2015_absolute_paths {
        return Ok(None);
    }
    let mut segments = alias_target.path.segments.iter().map(|segment| normalized_ident(&segment.ident)).collect::<Vec<_>>();
    if alias_target.path.leading_colon.is_some() {
        segments.insert(0, "crate".to_owned());
    }
    let Some(first) = segments.first() else {
        return Ok(None);
    };
    if first == "Self" || item.generics.type_params().any(|parameter| normalized_ident(&parameter.ident) == *first) {
        return Ok(None);
    }
    let Some(target_path) = resolve_path(context.module, &segments, false)? else {
        return Ok(None);
    };
    let alias = normalized_ident(&item.ident);
    let mut exported_path = context.module.to_vec();
    exported_path.push(alias.clone());
    let identity = format!(
        "public-type-alias:{}\0alias:{alias}\0visibility:{}\0cfg:{}\0ancestors:{}",
        target_path.join("::"),
        syntax_fingerprint(&item.vis),
        context.cfg.identity(),
        context.ancestors
    );
    Ok(Some(PendingPublicReexport {
        evidence: PublicReexportEvidence {
            exported_path,
            target_path,
            fingerprint: syntax_fingerprint(&identity),
            cfg: context.cfg.clone(),
            direct_exposure_cfg: context.direct_exposure_cfg,
        },
        cfg: context.cfg.clone(),
    }))
}

type ResolvedUseTargets = Vec<ResolvedPath>;

struct ResolvedPath {
    path: Vec<String>,
    aliases: Vec<String>,
    cfg: ProductionCfgContext,
}

struct ResolvedTraitPath {
    path: Option<Vec<String>>,
    aliases: Vec<String>,
    cfg: ProductionCfgContext,
}

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
        for target in resolve_use_aliases(evidence.target_path.clone(), &pending.cfg, resolutions) {
            let fingerprint = if target.aliases.is_empty() && target.path == evidence.target_path {
                evidence.fingerprint.clone()
            } else {
                syntax_fingerprint(&format!(
                    "{}\0resolved-target:{}\0aliases:{}",
                    evidence.fingerprint,
                    target.path.join("::"),
                    target.aliases.join("\0")
                ))
            };
            resolved.push(PublicReexportEvidence {
                exported_path: evidence.exported_path.clone(),
                target_path: target.path,
                fingerprint,
                direct_exposure_cfg: evidence.direct_exposure_cfg.as_ref().and_then(|direct| direct.conjoin(&target.cfg)),
                cfg: target.cfg,
            });
        }
    }
    resolved
}

pub(super) fn resolve_impl_signature_aliases(sites: &mut ConcreteStoreSignatureSites, resolutions: &[UseResolution]) {
    resolve_impl_signature_site_aliases(&mut sites.sqlite_store, resolutions);
    resolve_impl_signature_site_aliases(&mut sites.postgres_store, resolutions);
}

pub(super) fn resolve_binding_aliases(sites: &mut ConcreteStoreBindingSites, resolutions: &[UseResolution]) {
    resolve_store_binding_aliases(&mut sites.sqlite_store, resolutions);
    resolve_store_binding_aliases(&mut sites.postgres_store, resolutions);
}

fn resolve_store_binding_aliases(sites: &mut Vec<ConcreteStoreBindingSite>, resolutions: &[UseResolution]) {
    let mut resolved = Vec::new();
    for site in std::mem::take(sites) {
        if site.item_path.is_empty() {
            resolved.push(site);
            continue;
        }
        resolved.extend(
            resolve_use_aliases(site.item_path, &site.cfg, resolutions)
                .into_iter()
                .map(|target| ConcreteStoreBindingSite {
                    fingerprint: syntax_fingerprint(&format!(
                        "{}\0resolved-impl-self-type:{}\0aliases:{}",
                        site.fingerprint,
                        target.path.join("::"),
                        target.aliases.join("\0")
                    )),
                    item_path: target.path,
                    cfg: target.cfg,
                }),
        );
    }
    *sites = resolved;
}

fn resolve_impl_signature_site_aliases(sites: &mut Vec<ConcreteStoreSignatureSite>, resolutions: &[UseResolution]) {
    let mut resolved = Vec::new();
    for site in std::mem::take(sites) {
        for item in resolved_item_paths(&site, resolutions) {
            resolved.extend(
                resolved_trait_paths(&site, &item.cfg, resolutions)
                    .into_iter()
                    .map(|implemented_trait| resolved_signature_site(&site, &item, implemented_trait)),
            );
        }
    }
    *sites = resolved;
}

fn resolved_item_paths(site: &ConcreteStoreSignatureSite, resolutions: &[UseResolution]) -> ResolvedUseTargets {
    if site.impl_self_type && !site.item_path.is_empty() {
        resolve_use_aliases(site.item_path.clone(), &site.cfg, resolutions)
    } else {
        vec![ResolvedPath {
            path: site.item_path.clone(),
            aliases: Vec::new(),
            cfg: site.cfg.clone(),
        }]
    }
}

fn resolved_trait_paths(site: &ConcreteStoreSignatureSite, cfg: &ProductionCfgContext, resolutions: &[UseResolution]) -> Vec<ResolvedTraitPath> {
    site.required_trait_path.as_ref().map_or_else(
        || {
            vec![ResolvedTraitPath {
                path: None,
                aliases: Vec::new(),
                cfg: cfg.clone(),
            }]
        },
        |trait_path| {
            resolve_use_aliases(trait_path.clone(), cfg, resolutions)
                .into_iter()
                .map(|resolved| ResolvedTraitPath {
                    path: Some(resolved.path),
                    aliases: resolved.aliases,
                    cfg: resolved.cfg,
                })
                .collect()
        },
    )
}

fn resolved_signature_site(site: &ConcreteStoreSignatureSite, item: &ResolvedPath, implemented_trait: ResolvedTraitPath) -> ConcreteStoreSignatureSite {
    let item_identity = resolved_path_identity(&site.fingerprint, "impl-self-type", &site.item_path, &item.path, &item.aliases);
    let fingerprint = match (&site.required_trait_path, &implemented_trait.path) {
        (Some(original), Some(resolved)) => resolved_path_identity(&item_identity, "impl-trait", original, resolved, &implemented_trait.aliases),
        _ => item_identity,
    };
    ConcreteStoreSignatureSite {
        fingerprint,
        item_path: item.path.clone(),
        direct_exposure_cfg: site.direct_exposure_cfg.as_ref().and_then(|direct| direct.conjoin(&implemented_trait.cfg)),
        cfg: implemented_trait.cfg,
        impl_self_type: site.impl_self_type,
        required_trait_path: implemented_trait.path,
    }
}

fn resolved_path_identity(identity: &str, kind: &str, original: &[String], resolved: &[String], aliases: &[String]) -> String {
    if aliases.is_empty() && resolved == original {
        return identity.to_owned();
    }
    syntax_fingerprint(&format!("{identity}\0resolved-{kind}:{}\0aliases:{}", resolved.join("::"), aliases.join("\0")))
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
            self.targets.push(ResolvedPath {
                path,
                aliases: alias_fingerprints.clone(),
                cfg: cfg.clone(),
            });
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
            self.targets.push(ResolvedPath {
                path,
                aliases: alias_fingerprints.clone(),
                cfg: cfg.clone(),
            });
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
