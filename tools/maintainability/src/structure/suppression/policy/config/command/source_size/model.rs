use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::profile::SourceProfile;

pub(super) const POLICY_PATH: &str = "policy/maintainability/tooling-structure.json";
pub(super) const ROOT_MANIFEST: &str = "tools/maintainability/Cargo.toml";
pub(super) const ROOT_LOCKFILE: &str = "tools/maintainability/Cargo.lock";
pub(super) const SOURCE_ROOT: &str = "tools/maintainability/src";
pub(super) const PRODUCTION_FILE_LIMIT: usize = 800;
pub(super) const TEST_FILE_LIMIT: usize = 1_000;

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct ToolingStructureManifest {
    schema_version: u32,
    root_manifest: String,
    source_root: String,
    limits: Limits,
    component: Component,
    source_profile: SourceProfile,
    hotspots: Vec<Hotspot>,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Limits {
    production_file_physical_lines: usize,
    test_file_physical_lines: usize,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Component {
    id: String,
    physical_ceiling: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum HotspotStatus {
    Active,
    Resolved,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Hotspot {
    id: String,
    path: String,
    status: HotspotStatus,
    physical_ceiling: usize,
    issue: String,
    rationale: String,
}

impl ToolingStructureManifest {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        let manifest: Self = serde_json::from_slice(bytes).context("parse maintainability tooling structure policy")?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!("maintainability tooling structure policy schema must be 1");
        }
        if self.root_manifest != ROOT_MANIFEST || self.source_root != SOURCE_ROOT {
            bail!("maintainability tooling structure policy roots are immutable");
        }
        if self.limits.production_file_physical_lines != PRODUCTION_FILE_LIMIT || self.limits.test_file_physical_lines != TEST_FILE_LIMIT {
            bail!("maintainability tooling structure policy file limits are immutable");
        }
        if self.component.id != "maintainability-analyzer" {
            bail!("maintainability tooling structure policy component identity is immutable");
        }
        self.source_profile.validate()?;
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for hotspot in &self.hotspots {
            if hotspot.id.is_empty() || !ids.insert(hotspot.id.as_str()) {
                bail!("maintainability tooling hotspot IDs must be nonempty and unique");
            }
            if !is_source(&hotspot.path) || !paths.insert(hotspot.path.as_str()) {
                bail!("maintainability tooling hotspot paths must be unique Rust sources under {SOURCE_ROOT:?}");
            }
            if hotspot.issue.is_empty() || hotspot.rationale.trim().is_empty() {
                bail!("maintainability tooling hotspot {:?} requires issue and rationale evidence", hotspot.id);
            }
            match hotspot.status {
                HotspotStatus::Active if hotspot.physical_ceiling <= PRODUCTION_FILE_LIMIT => {
                    bail!("active maintainability tooling hotspot {:?} must exceed its ordinary file limit", hotspot.id);
                }
                HotspotStatus::Resolved if hotspot.physical_ceiling != 0 => {
                    bail!("resolved maintainability tooling hotspot {:?} must have a zero ceiling", hotspot.id);
                }
                HotspotStatus::Active | HotspotStatus::Resolved => {}
            }
        }
        Ok(())
    }

    pub(super) fn compare_current(&self, observed: &BTreeMap<String, usize>, observed_profile: &str, test_only: &BTreeSet<String>) -> Result<()> {
        self.source_profile.require_current(observed_profile)?;
        let total = observed
            .values()
            .try_fold(0_usize, |total, lines| total.checked_add(*lines).context("maintainability tooling line count overflow"))?;
        if total > self.component.physical_ceiling {
            bail!(
                "maintainability analyzer physical growth rejected: ceiling={}, observed={total}",
                self.component.physical_ceiling
            );
        }
        if total < self.component.physical_ceiling {
            bail!(
                "maintainability analyzer physical ceiling must be lowered from {} to {total} in this change",
                self.component.physical_ceiling
            );
        }

        let active = self.hotspots.iter().filter(|hotspot| hotspot.status == HotspotStatus::Active).collect::<Vec<_>>();
        let active_paths = active.iter().map(|hotspot| hotspot.path.as_str()).collect::<BTreeSet<_>>();
        let oversized = observed
            .iter()
            .filter(|(path, lines)| **lines > file_limit(path, test_only))
            .map(|(path, _)| path.as_str())
            .collect::<BTreeSet<_>>();
        if oversized != active_paths {
            bail!("maintainability tooling hotspot set mismatch: observed={oversized:?}, active={active_paths:?}");
        }
        for hotspot in active {
            let lines = observed.get(&hotspot.path).context("active maintainability tooling hotspot source is missing")?;
            if *lines > hotspot.physical_ceiling {
                bail!(
                    "maintainability tooling hotspot {:?} growth rejected: ceiling={}, observed={lines}",
                    hotspot.id,
                    hotspot.physical_ceiling
                );
            }
            if *lines < hotspot.physical_ceiling {
                bail!(
                    "maintainability tooling hotspot {:?} ceiling must be lowered from {} to {lines} in this change",
                    hotspot.id,
                    hotspot.physical_ceiling
                );
            }
        }
        Ok(())
    }

    pub(super) fn compare_previous(&self, previous: &Self) -> Result<()> {
        if self.schema_version != previous.schema_version
            || self.root_manifest != previous.root_manifest
            || self.source_root != previous.source_root
            || self.limits != previous.limits
            || self.component.id != previous.component.id
        {
            bail!("maintainability tooling structure policy identity and limits are immutable");
        }
        if self.component.physical_ceiling > previous.component.physical_ceiling {
            bail!("maintainability tooling component ceiling cannot increase");
        }
        self.source_profile.compare_previous(&previous.source_profile)?;
        let current = hotspot_map(&self.hotspots)?;
        let prior = hotspot_map(&previous.hotspots)?;
        if current.keys().collect::<BTreeSet<_>>() != prior.keys().collect::<BTreeSet<_>>() {
            bail!("maintainability tooling hotspot IDs are append-only and cannot be removed or added");
        }
        for (id, hotspot) in current {
            let prior = prior.get(id).context("previous maintainability tooling hotspot is missing")?;
            if hotspot.path != prior.path || hotspot.issue != prior.issue || hotspot.rationale != prior.rationale {
                bail!("maintainability tooling hotspot {id:?} identity and evidence are immutable");
            }
            if hotspot.physical_ceiling > prior.physical_ceiling {
                bail!("maintainability tooling hotspot {id:?} ceiling cannot increase");
            }
            if prior.status == HotspotStatus::Resolved && hotspot.status != HotspotStatus::Resolved {
                bail!("resolved maintainability tooling hotspot {id:?} cannot be reactivated");
            }
        }
        Ok(())
    }
}

fn hotspot_map(hotspots: &[Hotspot]) -> Result<BTreeMap<&str, &Hotspot>> {
    let mut mapped = BTreeMap::new();
    for hotspot in hotspots {
        if mapped.insert(hotspot.id.as_str(), hotspot).is_some() {
            bail!("maintainability tooling hotspot IDs must be unique");
        }
    }
    Ok(mapped)
}

pub(super) fn is_source(path: &str) -> bool {
    path.starts_with(&format!("{SOURCE_ROOT}/")) && std::path::Path::new(path).extension().is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

pub(super) fn file_limit(path: &str, test_only: &BTreeSet<String>) -> usize {
    if test_only.contains(path) { TEST_FILE_LIMIT } else { PRODUCTION_FILE_LIMIT }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(component: usize, hotspot: usize, status: &str) -> ToolingStructureManifest {
        let source = format!(
            r#"{{
                "schema_version": 1,
                "root_manifest": "tools/maintainability/Cargo.toml",
                "source_root": "tools/maintainability/src",
                "limits": {{"production_file_physical_lines": 800, "test_file_physical_lines": 1000}},
                "component": {{"id": "maintainability-analyzer", "physical_ceiling": {component}}},
                "source_profile": {{
                    "current_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "preapproved_next_sha256": null,
                    "retired_sha256": []
                }},
                "hotspots": [{{
                    "id": "tooling.large",
                    "path": "tools/maintainability/src/large.rs",
                    "status": "{status}",
                    "physical_ceiling": {hotspot},
                    "issue": "https://github.com/gearboxlogic/localhold/issues/124",
                    "rationale": "Legacy maintainability analyzer hotspot scheduled for Phase 0 decomposition."
                }}]
            }}"#
        );
        ToolingStructureManifest::parse(source.as_bytes()).expect("tooling structure manifest")
    }

    #[test]
    fn current_counts_must_ratchet_every_shrink() {
        let current = manifest(900, 900, "active");
        current
            .compare_current(
                &BTreeMap::from([("tools/maintainability/src/large.rs".to_owned(), 900)]),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &BTreeSet::new(),
            )
            .expect("exact current counts");
        let error = current
            .compare_current(
                &BTreeMap::from([("tools/maintainability/src/large.rs".to_owned(), 850)]),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &BTreeSet::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("ceiling must be lowered"), "{error:#}");

        let lowered = manifest(850, 850, "active");
        lowered.compare_previous(&current).expect("downward ratchet");
        assert!(current.compare_previous(&lowered).is_err());
    }

    #[test]
    fn resolution_is_one_way_and_prevents_path_resurrection() {
        let active = manifest(900, 900, "active");
        let resolved = manifest(0, 0, "resolved");
        resolved.compare_previous(&active).expect("resolve hotspot");
        assert!(active.compare_previous(&resolved).is_err());
        resolved
            .compare_current(&BTreeMap::new(), "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &BTreeSet::new())
            .expect("resolved path may be removed");
        let resurrected = manifest(900, 0, "resolved");
        let error = resurrected
            .compare_current(
                &BTreeMap::from([("tools/maintainability/src/large.rs".to_owned(), 900)]),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &BTreeSet::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("hotspot set mismatch"), "{error:#}");
    }
}
