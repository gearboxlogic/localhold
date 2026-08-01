use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::model::{BaselineReference, Disposition, SourceBaseline, SourceException, SourceGovernance, Status};
use super::{load_policy_file, require_id, require_text};
use crate::structure::suppression::{SourceCategory, SourceSuppression};

pub(in crate::structure) type SourceCounts = BTreeMap<(SourceCategory, String), usize>;

pub(super) fn load_baselines(workspace: &std::path::Path, references: &[BaselineReference]) -> Result<SourceCounts> {
    collect_baselines(references, |path| load_policy_file(workspace, path))
}

pub(super) fn collect_baselines(references: &[BaselineReference], mut load: impl FnMut(&str) -> Result<SourceBaseline>) -> Result<SourceCounts> {
    let mut categories = BTreeSet::new();
    let mut limits = BTreeMap::new();
    for reference in references {
        if !categories.insert(reference.category) {
            bail!("duplicate lint-suppression source baseline category");
        }
        let baseline = load(&reference.path)?;
        validate_baseline(reference, &baseline)?;
        for entry in baseline.entries {
            let key = (baseline.category, entry.id);
            if limits.insert(key, entry.max_occurrences).is_some() {
                bail!("duplicate lint-suppression source baseline signature");
            }
        }
    }
    let expected = [SourceCategory::Production, SourceCategory::Test, SourceCategory::Benchmark]
        .into_iter()
        .collect::<BTreeSet<_>>();
    if categories != expected {
        bail!("lint-suppression source baselines must cover production, test, and benchmark categories exactly");
    }
    Ok(limits)
}

pub(super) fn validate_governance(entries: &[SourceGovernance]) -> Result<()> {
    let categories = entries.iter().map(|entry| entry.category).collect::<BTreeSet<_>>();
    let expected = [SourceCategory::Production, SourceCategory::Test, SourceCategory::Benchmark]
        .into_iter()
        .collect::<BTreeSet<_>>();
    if entries.len() != expected.len() || categories != expected {
        bail!("lint-suppression source governance must cover each category exactly once");
    }
    for entry in entries {
        for (label, value) in [
            ("owner", entry.owner.as_str()),
            ("issue", entry.issue.as_str()),
            ("rationale", entry.rationale.as_str()),
            ("safety invariant", entry.safety_invariant.as_str()),
            ("alternatives considered", entry.alternatives_considered.as_str()),
            ("evidence", entry.evidence.as_str()),
            ("re-review phase", entry.re_review_phase.as_str()),
        ] {
            require_text("source governance", label, value)?;
        }
    }
    Ok(())
}

pub(super) fn validate_exceptions(entries: &[SourceException]) -> Result<()> {
    let mut ids = BTreeSet::new();
    let mut sites = BTreeSet::new();
    for entry in entries {
        require_id("source exception", &entry.id)?;
        require_source_id(&entry.source_id)?;
        if !ids.insert(entry.id.as_str()) {
            bail!("duplicate lint-suppression source exception ID {:?}", entry.id);
        }
        if !sites.insert((entry.category, entry.source_id.as_str())) {
            bail!("duplicate lint-suppression source exception for {:?}", entry.source_id);
        }
        if entry.max_occurrences == 0 {
            bail!("lint-suppression source exception {:?} must authorize a positive occurrence count", entry.id);
        }
        for (label, value) in [
            ("owner", entry.owner.as_str()),
            ("issue", entry.issue.as_str()),
            ("pull request", entry.pull_request.as_str()),
            ("rationale", entry.rationale.as_str()),
            ("safety invariant", entry.safety_invariant.as_str()),
            ("alternatives considered", entry.alternatives_considered.as_str()),
            ("evidence", entry.evidence.as_str()),
            ("re-review phase", entry.re_review_phase.as_str()),
        ] {
            require_text(&entry.id, label, value)?;
        }
        validate_disposition(&entry.id, entry.disposition, entry.removal_issue.as_deref(), entry.removal_phase.as_deref())?;
    }
    Ok(())
}

pub(super) fn compare_current(sites: &[SourceSuppression], baseline: &SourceCounts, exceptions: &[SourceException], initial_adoption: bool) -> Result<SourceCounts> {
    let observed = observed_counts(sites)?;
    let mut allowed = baseline.clone();
    for exception in exceptions.iter().filter(|entry| entry.status == Status::Active) {
        let key = (exception.category, exception.source_id.clone());
        let count = allowed.entry(key).or_default();
        *count = count.checked_add(exception.max_occurrences).context("lint-suppression source exception count overflow")?;
    }
    if initial_adoption {
        if exceptions.iter().any(|entry| entry.status != Status::Active) {
            bail!("initial lint-suppression policy adoption requires active source exceptions");
        }
        require_count_subset("initial adoption policy", &observed, &allowed)?;
        for exception in exceptions {
            let site = (exception.category, exception.source_id.clone());
            let baseline_count = baseline.get(&site).copied().unwrap_or_default();
            let expected_count = baseline_count
                .checked_add(exception.max_occurrences)
                .context("initial lint-suppression source exception count overflow")?;
            let observed_count = observed.get(&site).copied().unwrap_or_default();
            if observed_count != expected_count {
                bail!(
                    "initial lint-suppression source exception capacity must exactly match the adopted source inventory: site={site:?}, baseline={baseline_count}, observed={observed_count}, added={}",
                    exception.max_occurrences
                );
            }
        }
        return Ok(observed);
    }
    require_count_subset("reviewed source policy", &observed, &allowed)?;
    Ok(observed)
}

pub(super) fn require_count_subset(reference: &str, current: &SourceCounts, allowed: &SourceCounts) -> Result<()> {
    for (site, current_count) in current {
        let allowed_count = allowed.get(site).copied().unwrap_or_default();
        if *current_count > allowed_count {
            bail!(
                "lint-suppression source signature is new, moved across governed identity, or resurrected beyond {reference}: site={site:?}, current={current_count}, allowed={allowed_count}"
            );
        }
    }
    Ok(())
}

pub(super) fn added_exception_capacity(current: &[SourceException], previous: &[SourceException]) -> Result<SourceCounts> {
    let mut capacity = BTreeMap::new();
    for entry in current.iter().skip(previous.len()).filter(|entry| entry.status == Status::Active) {
        let count = capacity.entry((entry.category, entry.source_id.clone())).or_insert(0_usize);
        *count = count.checked_add(entry.max_occurrences).context("new lint-suppression source exception count overflow")?;
    }
    Ok(capacity)
}

fn observed_counts(sites: &[SourceSuppression]) -> Result<SourceCounts> {
    let mut observed = BTreeMap::new();
    for site in sites {
        if site.level != "expect" {
            bail!(
                "source lint suppression {} uses {:?}; only reason-bearing expect attributes are permitted",
                site.id,
                site.level
            );
        }
        if site.reason.trim().is_empty() {
            bail!("source lint suppression {} has no reviewable reason", site.id);
        }
        if site.macro_carried {
            bail!("source lint suppression {} is carried by a macro and has no intrinsic expansion policy", site.id);
        }
        require_source_id(&site.id)?;
        let count = observed.entry((site.category, site.id.clone())).or_insert(0_usize);
        *count = count.checked_add(1).context("lint-suppression source inventory count overflow")?;
    }
    Ok(observed)
}

fn validate_baseline(reference: &BaselineReference, baseline: &SourceBaseline) -> Result<()> {
    if baseline.schema_version != 1 {
        bail!("unsupported lint-suppression source baseline schema {}", baseline.schema_version);
    }
    if baseline.category != reference.category {
        bail!("lint-suppression source baseline category does not match its policy reference");
    }
    if baseline.entries.len() != reference.expected_signatures {
        bail!("lint-suppression source baseline signature count does not match its policy reference");
    }
    if baseline.entries.windows(2).any(|pair| pair[0] >= pair[1]) {
        bail!("lint-suppression source baseline entries must be strictly sorted and unique");
    }
    let mut sites = 0_usize;
    for entry in &baseline.entries {
        require_source_id(&entry.id)?;
        if entry.max_occurrences == 0 {
            bail!("lint-suppression source baseline entries must have positive occurrence counts");
        }
        sites = sites.checked_add(entry.max_occurrences).context("lint-suppression source baseline site count overflow")?;
    }
    if sites != reference.expected_sites {
        bail!("lint-suppression source baseline site count does not match its policy reference");
    }
    Ok(())
}

fn require_source_id(value: &str) -> Result<()> {
    let Some(digest) = value.strip_prefix("source.") else {
        bail!("lint-suppression source ID must start with 'source.'");
    };
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) {
        bail!("lint-suppression source ID must contain one lowercase SHA-256 digest");
    }
    Ok(())
}

fn validate_disposition(id: &str, disposition: Disposition, removal_issue: Option<&str>, removal_phase: Option<&str>) -> Result<()> {
    match disposition {
        Disposition::Permanent if removal_issue.is_some() || removal_phase.is_some() => {
            bail!("permanent lint-suppression exception {id:?} cannot carry temporary removal fields");
        }
        Disposition::Temporary => {
            require_text(id, "removal issue", removal_issue.unwrap_or_default())?;
            require_text(id, "removal phase", removal_phase.unwrap_or_default())?;
        }
        Disposition::Permanent => {}
    }
    Ok(())
}
