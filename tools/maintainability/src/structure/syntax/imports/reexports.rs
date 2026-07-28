use std::collections::BTreeSet;

use crate::scan::syntax_fingerprint;

use super::PublicReexportEvidence;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct UseResolution {
    pub(super) exported_path: Vec<String>,
    pub(super) target_path: Vec<String>,
    pub(super) fingerprint: String,
}

type ResolvedUseTargets = Vec<(Vec<String>, Vec<String>)>;

pub(super) fn resolve_public_reexport_aliases(reexports: &mut Vec<PublicReexportEvidence>, resolutions: &[UseResolution]) {
    let unresolved = std::mem::take(reexports);
    for evidence in unresolved {
        let mut targets = Vec::new();
        resolve_use_aliases(evidence.target_path.clone(), resolutions, &mut BTreeSet::new(), &mut Vec::new(), &mut targets);
        for (target_path, alias_fingerprints) in targets {
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
            reexports.push(PublicReexportEvidence {
                exported_path: evidence.exported_path.clone(),
                target_path,
                fingerprint,
            });
        }
    }
}

fn resolve_use_aliases(
    path: Vec<String>,
    resolutions: &[UseResolution],
    visited: &mut BTreeSet<Vec<String>>,
    alias_fingerprints: &mut Vec<String>,
    targets: &mut ResolvedUseTargets,
) {
    if !visited.insert(path.clone()) {
        targets.push((path, alias_fingerprints.clone()));
        return;
    }
    let rewritten = resolutions
        .iter()
        .filter_map(|resolution| rewrite_use_target(&path, resolution).map(|target| (target, &resolution.fingerprint)))
        .filter(|(target, _)| target != &path)
        .collect::<Vec<_>>();
    if rewritten.is_empty() {
        targets.push((path.clone(), alias_fingerprints.clone()));
    } else {
        for (target, fingerprint) in rewritten {
            alias_fingerprints.push(fingerprint.clone());
            resolve_use_aliases(target, resolutions, &mut visited.clone(), alias_fingerprints, targets);
            alias_fingerprints.pop();
        }
    }
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
