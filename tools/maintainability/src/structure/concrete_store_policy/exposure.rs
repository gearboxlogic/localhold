use std::collections::BTreeSet;

use crate::scan::syntax_fingerprint;
use crate::structure::classify::{FileMeasurement, Inventory};
use crate::structure::syntax::{ProductionCfgContext, PublicReexportEvidence, TypeDeclarationEvidence, TypeDeclarationKind};

use super::PathAttribution;

pub(super) struct TraitExposureEvidence {
    pub(super) fingerprint: String,
    pub(super) cfg: ProductionCfgContext,
}

pub(super) fn public_reexport_evidence(
    inventory: &Inventory,
    paths: PathAttribution<'_>,
    target_item: &[String],
    target_cfg: &ProductionCfgContext,
    production_targets: &[String],
) -> Vec<String> {
    if target_item.is_empty() {
        return Vec::new();
    }
    let mut evidence = Vec::new();
    for target in production_targets {
        let reexports = inventory
            .files
            .iter()
            .filter(|file| file.production_targets.contains(target))
            .flat_map(|file| file.production_public_reexports.iter().map(move |reexport| (file, reexport)))
            .collect::<Vec<_>>();
        evidence.extend(
            reexports
                .iter()
                .filter(|(_, reexport)| reexport_reaches_item(reexport, &reexports, target_item, target_cfg))
                .map(|(file, reexport)| format!("{}:{}:{}", paths.site_path(&file.path).len(), paths.site_path(&file.path), reexport.fingerprint)),
        );
    }
    evidence.sort();
    evidence.dedup();
    evidence
}

pub(super) fn type_declaration_evidence(
    inventory: &Inventory,
    paths: PathAttribution<'_>,
    target_item: &[String],
    target_cfg: &ProductionCfgContext,
    production_targets: &[String],
) -> Vec<String> {
    if target_item.is_empty() {
        return Vec::new();
    }
    let mut evidence = Vec::new();
    for target in production_targets {
        evidence.extend(inventory.files.iter().filter(|file| file.production_targets.contains(target)).flat_map(|file| {
            file.production_type_declarations
                .iter()
                .filter(move |declaration| declaration_applies_to_impl(declaration, target_item, target_cfg))
                .map(move |declaration| format!("{}:{}:{}", paths.site_path(&file.path).len(), paths.site_path(&file.path), declaration.fingerprint))
        }));
    }
    evidence.sort();
    evidence.dedup();
    evidence
}

pub(super) fn trait_exposure_evidence(
    inventory: &Inventory,
    paths: PathAttribution<'_>,
    trait_path: &[String],
    target_cfg: &ProductionCfgContext,
    production_targets: &[String],
) -> Vec<TraitExposureEvidence> {
    let mut evidence = Vec::new();
    for target in production_targets {
        let files = inventory.files.iter().filter(|file| file.production_targets.contains(target)).collect::<Vec<_>>();
        let declarations = files
            .iter()
            .flat_map(|file| {
                file.production_type_declarations
                    .iter()
                    .filter(|declaration| declaration.kind == TypeDeclarationKind::Trait && declaration.item_path == trait_path)
                    .map(move |declaration| (*file, declaration))
            })
            .collect::<Vec<_>>();
        if declarations.is_empty() {
            evidence.push(TraitExposureEvidence {
                fingerprint: syntax_fingerprint(&format!("external-or-unresolved-trait:{}", trait_path.join("::"))),
                cfg: target_cfg.clone(),
            });
            continue;
        }
        let reexports = files
            .iter()
            .flat_map(|file| file.production_public_reexports.iter().map(move |reexport| (*file, reexport)))
            .collect::<Vec<_>>();
        for (file, declaration) in declarations {
            let Some(declaration_cfg) = declaration.cfg.conjoin(target_cfg) else {
                continue;
            };
            let declaration_identity = format!("{}:{}:{}", paths.site_path(&file.path).len(), paths.site_path(&file.path), declaration.fingerprint);
            if let Some(direct_cfg) = declaration.direct_exposure_cfg.as_ref().and_then(|direct| direct.conjoin(&declaration_cfg)) {
                evidence.push(TraitExposureEvidence {
                    fingerprint: syntax_fingerprint(&format!("trait-declaration:{declaration_identity}\0direct:{}", direct_cfg.identity())),
                    cfg: direct_cfg,
                });
            }
            evidence.extend(reexports.iter().filter_map(|(reexport_file, reexport)| {
                let cfg = reexport.direct_exposure_cfg.as_ref()?.conjoin(&declaration_cfg)?;
                reexport_reaches_item(reexport, &reexports, trait_path, &cfg).then(|| TraitExposureEvidence {
                    fingerprint: syntax_fingerprint(&format!(
                        "trait-declaration:{declaration_identity}\0reexport:{}:{}:{}",
                        paths.site_path(&reexport_file.path).len(),
                        paths.site_path(&reexport_file.path),
                        reexport.fingerprint
                    )),
                    cfg,
                })
            }));
        }
    }
    evidence.sort_by(|left, right| left.fingerprint.cmp(&right.fingerprint).then_with(|| left.cfg.cmp(&right.cfg)));
    evidence.dedup_by(|left, right| left.fingerprint == right.fingerprint && left.cfg == right.cfg);
    evidence
}

fn declaration_applies_to_impl(declaration: &TypeDeclarationEvidence, target_item: &[String], target_cfg: &ProductionCfgContext) -> bool {
    declaration.kind == TypeDeclarationKind::Type && declaration.item_path == target_item && declaration.cfg.conjoin(target_cfg).is_some()
}

fn reexport_reaches_item(
    candidate: &PublicReexportEvidence,
    reexports: &[(&FileMeasurement, &PublicReexportEvidence)],
    target_item: &[String],
    target_cfg: &ProductionCfgContext,
) -> bool {
    let Some(candidate_cfg) = candidate.direct_exposure_cfg.as_ref().and_then(|direct| direct.conjoin(target_cfg)) else {
        return false;
    };
    let mut pending = vec![(candidate.target_path.clone(), candidate_cfg, BTreeSet::new())];
    while let Some((path, cfg, mut visited)) = pending.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        if reexport_applies_to_item(&path, target_item) {
            return true;
        }
        pending.extend(reexports.iter().filter_map(|(_, reexport)| {
            let compatible_cfg = cfg.conjoin(&reexport.cfg)?;
            resolve_reexport_target(&path, reexport).map(|path| (path, compatible_cfg, visited.clone()))
        }));
    }
    false
}

fn resolve_reexport_target(path: &[String], reexport: &PublicReexportEvidence) -> Option<Vec<String>> {
    let exported_glob = reexport.exported_path.last().is_some_and(|segment| segment == "*");
    let exported_prefix = reexport.exported_path.strip_suffix(&["*".to_owned()]).unwrap_or(&reexport.exported_path);
    if !path.starts_with(exported_prefix) || exported_glob && path.len() == exported_prefix.len() {
        return None;
    }
    let target_prefix = reexport.target_path.strip_suffix(&["*".to_owned()]).unwrap_or(&reexport.target_path);
    let mut resolved = target_prefix.to_vec();
    resolved.extend_from_slice(&path[exported_prefix.len()..]);
    (resolved != path).then_some(resolved)
}

fn reexport_applies_to_item(target_path: &[String], item: &[String]) -> bool {
    let target_without_glob = if target_path.last().is_some_and(|segment| segment == "*") {
        &target_path[..target_path.len() - 1]
    } else {
        target_path
    };
    target_without_glob == item
        || !target_without_glob.is_empty() && item.starts_with(target_without_glob)
        || target_without_glob.len() == item.len().saturating_add(1) && target_without_glob.starts_with(item)
}
