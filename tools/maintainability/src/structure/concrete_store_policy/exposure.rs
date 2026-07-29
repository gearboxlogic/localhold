use std::collections::{BTreeMap, BTreeSet};

use crate::scan::syntax_fingerprint;
use crate::structure::classify::{FileMeasurement, Inventory};
use crate::structure::syntax::{ProductionCfgContext, PublicReexportEvidence, TypeDeclarationEvidence, TypeDeclarationKind};

use super::PathAttribution;

#[derive(Clone)]
pub(super) struct TraitExposureEvidence {
    pub(super) fingerprint: String,
    pub(super) cfg: ProductionCfgContext,
}

struct ActiveReexport<'a> {
    file: &'a FileMeasurement,
    evidence: &'a PublicReexportEvidence,
    cfg: ProductionCfgContext,
    direct_exposure_cfg: Option<ProductionCfgContext>,
    trait_fingerprint: Option<String>,
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
        let declared = inventory
            .files
            .iter()
            .filter(|file| file.production_targets.contains(target))
            .flat_map(|file| file.production_public_reexports.iter().map(move |reexport| (file, reexport)))
            .collect::<Vec<_>>();
        let reexports = active_reexports(inventory, paths, target, &declared);
        let mut search = ReexportSearch::new(&reexports, target_item);
        evidence.extend(
            reexports
                .iter()
                .filter(|reexport| reexport.direct_exposure_cfg.is_some() && search.reaches(reexport, target_cfg))
                .map(|reexport| {
                    let fingerprint = reexport.trait_fingerprint.as_ref().map_or_else(
                        || reexport.evidence.fingerprint.clone(),
                        |trait_fingerprint| syntax_fingerprint(&format!("reexport:{}\0required-trait-exposure:{trait_fingerprint}", reexport.evidence.fingerprint)),
                    );
                    format!("{}:{}:{fingerprint}", paths.site_path(&reexport.file.path).len(), paths.site_path(&reexport.file.path))
                }),
        );
    }
    evidence.sort();
    evidence.dedup();
    evidence
}

fn active_reexports<'a>(
    inventory: &'a Inventory,
    paths: PathAttribution<'_>,
    target: &String,
    declared: &[(&'a FileMeasurement, &'a PublicReexportEvidence)],
) -> Vec<ActiveReexport<'a>> {
    let mut active = Vec::new();
    let mut trait_exposures = BTreeMap::<(Vec<String>, ProductionCfgContext), Vec<TraitExposureEvidence>>::new();
    for (file, reexport) in declared {
        let Some(trait_path) = reexport.required_trait_path.as_ref() else {
            active.push(ActiveReexport {
                file,
                evidence: reexport,
                cfg: reexport.cfg.clone(),
                direct_exposure_cfg: reexport.direct_exposure_cfg.clone(),
                trait_fingerprint: None,
            });
            continue;
        };
        let required = trait_exposures
            .entry((trait_path.clone(), reexport.cfg.clone()))
            .or_insert_with(|| trait_exposure_evidence(inventory, paths, trait_path, &reexport.cfg, std::slice::from_ref(target)));
        active.extend(required.iter().filter_map(|trait_exposure| {
            let cfg = reexport.cfg.conjoin(&trait_exposure.cfg)?;
            Some(ActiveReexport {
                file,
                evidence: reexport,
                direct_exposure_cfg: reexport.direct_exposure_cfg.as_ref().and_then(|direct| direct.conjoin(&cfg)),
                cfg,
                trait_fingerprint: Some(trait_exposure.fingerprint.clone()),
            })
        }));
    }
    active
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
            .filter(|(_, reexport)| reexport.required_trait_path.is_none())
            .map(|(file, reexport)| ActiveReexport {
                file,
                evidence: reexport,
                cfg: reexport.cfg.clone(),
                direct_exposure_cfg: reexport.direct_exposure_cfg.clone(),
                trait_fingerprint: None,
            })
            .collect::<Vec<_>>();
        let mut search = ReexportSearch::new(&reexports, trait_path);
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
            evidence.extend(reexports.iter().filter_map(|reexport| {
                let cfg = reexport.direct_exposure_cfg.as_ref()?.conjoin(&declaration_cfg)?;
                search.reaches(reexport, &cfg).then(|| TraitExposureEvidence {
                    fingerprint: syntax_fingerprint(&format!(
                        "trait-declaration:{declaration_identity}\0reexport:{}:{}:{}",
                        paths.site_path(&reexport.file.path).len(),
                        paths.site_path(&reexport.file.path),
                        reexport.evidence.fingerprint
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

fn index_reexports(reexports: &[ActiveReexport<'_>]) -> BTreeMap<Vec<String>, Vec<usize>> {
    let mut index = BTreeMap::<Vec<String>, Vec<usize>>::new();
    for (position, reexport) in reexports.iter().enumerate() {
        let exported_prefix = reexport.evidence.exported_path.strip_suffix(&["*".to_owned()]).unwrap_or(&reexport.evidence.exported_path);
        index.entry(exported_prefix.to_vec()).or_default().push(position);
    }
    index
}

type ReexportState = (Vec<String>, ProductionCfgContext);
type ReexportTransition = (ProductionCfgContext, Vec<String>);

enum Reachability {
    Reaches,
    DoesNotReach,
    Cycle,
}

struct ReexportSearch<'source, 'search> {
    reexports: &'search [ActiveReexport<'source>],
    index: BTreeMap<Vec<String>, Vec<usize>>,
    target_item: &'search [String],
    memo: BTreeMap<ReexportState, bool>,
    active: BTreeSet<ReexportState>,
}

impl<'source, 'search> ReexportSearch<'source, 'search> {
    fn new(reexports: &'search [ActiveReexport<'source>], target_item: &'search [String]) -> Self {
        Self {
            reexports,
            index: index_reexports(reexports),
            target_item,
            memo: BTreeMap::new(),
            active: BTreeSet::new(),
        }
    }

    fn reaches(&mut self, candidate: &ActiveReexport<'_>, target_cfg: &ProductionCfgContext) -> bool {
        let Some(candidate_cfg) = candidate.direct_exposure_cfg.as_ref().and_then(|direct| direct.conjoin(target_cfg)) else {
            return false;
        };
        matches!(self.path_reaches(&candidate.evidence.target_path, &candidate_cfg), Reachability::Reaches)
    }

    fn path_reaches(&mut self, path: &[String], cfg: &ProductionCfgContext) -> Reachability {
        if reexport_applies_to_item(path, self.target_item) {
            return Reachability::Reaches;
        }
        let state = (path.to_owned(), cfg.clone());
        if let Some(reaches) = self.memo.get(&state) {
            return if *reaches { Reachability::Reaches } else { Reachability::DoesNotReach };
        }
        if !self.active.insert(state.clone()) {
            return Reachability::Cycle;
        }
        let mut cycle = false;
        for (compatible_cfg, resolved) in self.transitions(path, cfg) {
            match self.path_reaches(&resolved, &compatible_cfg) {
                Reachability::Reaches => return self.finish(state, true),
                Reachability::Cycle => cycle = true,
                Reachability::DoesNotReach => {}
            }
        }
        self.active.remove(&state);
        if cycle {
            Reachability::Cycle
        } else {
            self.memo.insert(state, false);
            Reachability::DoesNotReach
        }
    }

    fn transitions(&self, path: &[String], cfg: &ProductionCfgContext) -> Vec<ReexportTransition> {
        (1..=path.len())
            .flat_map(|prefix_length| self.index.get(&path[..prefix_length]).into_iter().flatten())
            .filter_map(|position| {
                let reexport = &self.reexports[*position];
                Some((cfg.conjoin(&reexport.cfg)?, resolve_reexport_target(path, reexport.evidence)?))
            })
            .collect()
    }

    fn finish(&mut self, state: ReexportState, reaches: bool) -> Reachability {
        self.active.remove(&state);
        self.memo.insert(state, reaches);
        if reaches { Reachability::Reaches } else { Reachability::DoesNotReach }
    }
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
