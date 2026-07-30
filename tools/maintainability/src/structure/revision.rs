use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::Value;

const BASE_REVISION_ENV: &str = "LOCALHOLD_MAINTAINABILITY_BASE_REV";
const ZERO_REVISION: &str = "0000000000000000000000000000000000000000";

pub(super) fn maintainability_base_revision() -> Result<Option<String>> {
    let configured = normalize_optional_revision(env::var(BASE_REVISION_ENV).ok().as_deref())?;
    let event_path = env::var_os("GITHUB_EVENT_PATH");
    let head = env::var("GITHUB_SHA").ok();
    if !github_actions_environment(env::var("GITHUB_ACTIONS").ok().as_deref(), event_path.is_some(), head.is_some())? {
        return Ok(configured);
    }

    let event_path = event_path.map(PathBuf::from).context("GitHub Actions maintainability checks require GITHUB_EVENT_PATH")?;
    let event_bytes = fs::read(&event_path).with_context(|| format!("read GitHub event {}", event_path.display()))?;
    let event: Value = serde_json::from_slice(&event_bytes).context("parse GitHub event for maintainability base revision")?;
    let event_base = event_base_revision(&event)?;
    let head = head.context("GitHub Actions maintainability checks require GITHUB_SHA")?;
    validate_revision(&head, "GitHub head revision")?;
    select_base_revision(configured.as_deref(), Some(event_base), Some(&head))
}

fn github_actions_environment(marker: Option<&str>, has_event_path: bool, has_head: bool) -> Result<bool> {
    let has_any_marker = marker.is_some() || has_event_path || has_head;
    if !has_any_marker {
        return Ok(false);
    }
    if marker.is_some_and(|value| value != "true") || !has_event_path || !has_head {
        bail!("incomplete or invalid GitHub Actions environment cannot disable maintainability revision comparison");
    }
    Ok(true)
}

fn select_base_revision(configured: Option<&str>, github_base: Option<&str>, github_head: Option<&str>) -> Result<Option<String>> {
    let Some(github_base) = github_base else {
        return Ok(configured.map(str::to_owned));
    };
    validate_revision(github_base, "GitHub event base revision")?;
    let github_head = github_head.context("GitHub event base revision requires a head revision")?;
    validate_revision(github_head, "GitHub head revision")?;
    if github_base == github_head {
        bail!("GitHub maintainability base revision must differ from the checked head");
    }
    if configured.is_some_and(|revision| revision != github_base) {
        bail!("configured maintainability base revision differs from the GitHub event base revision");
    }
    Ok(Some(github_base.to_owned()))
}

fn event_base_revision(event: &Value) -> Result<&str> {
    let revision = event
        .pointer("/pull_request/base/sha")
        .and_then(Value::as_str)
        .or_else(|| event.get("before").and_then(Value::as_str))
        .context("GitHub event has no pull-request base or previous push revision")?;
    if revision == ZERO_REVISION {
        bail!("GitHub event has no usable previous revision for maintainability comparison");
    }
    Ok(revision)
}

fn normalize_optional_revision(revision: Option<&str>) -> Result<Option<String>> {
    let Some(revision) = revision.filter(|revision| !revision.is_empty() && *revision != ZERO_REVISION) else {
        return Ok(None);
    };
    validate_revision(revision, "configured maintainability base revision")?;
    Ok(Some(revision.to_owned()))
}

fn validate_revision(revision: &str, label: &str) -> Result<()> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} must be a full Git commit hash");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const BASE: &str = "1111111111111111111111111111111111111111";
    const HEAD: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn github_event_base_cannot_be_disabled_or_replaced() {
        assert_eq!(select_base_revision(None, Some(BASE), Some(HEAD)).unwrap().as_deref(), Some(BASE));
        assert_eq!(select_base_revision(Some(BASE), Some(BASE), Some(HEAD)).unwrap().as_deref(), Some(BASE));
        assert!(select_base_revision(Some(HEAD), Some(BASE), Some(HEAD)).is_err());
        assert!(select_base_revision(Some(BASE), Some(BASE), Some(BASE)).is_err());
    }

    #[test]
    fn github_event_inputs_keep_actions_mode_fail_closed_without_the_marker() {
        assert!(github_actions_environment(None, true, true).unwrap());
        assert!(github_actions_environment(Some("true"), true, true).unwrap());
        assert!(!github_actions_environment(None, false, false).unwrap());
        assert!(github_actions_environment(Some("false"), true, true).is_err());
        assert!(github_actions_environment(Some("true"), false, true).is_err());
        assert!(github_actions_environment(None, true, false).is_err());
    }

    #[test]
    fn pull_request_and_push_events_supply_exact_bases() {
        assert_eq!(event_base_revision(&json!({"pull_request": {"base": {"sha": BASE}}})).unwrap(), BASE);
        assert_eq!(event_base_revision(&json!({"before": BASE})).unwrap(), BASE);
        assert!(event_base_revision(&json!({"before": ZERO_REVISION})).is_err());
        assert!(event_base_revision(&json!({})).is_err());
    }

    #[test]
    fn local_revision_deferral_remains_explicit() {
        assert_eq!(normalize_optional_revision(None).unwrap(), None);
        assert_eq!(normalize_optional_revision(Some("")).unwrap(), None);
        assert_eq!(normalize_optional_revision(Some(ZERO_REVISION)).unwrap(), None);
        assert_eq!(normalize_optional_revision(Some(BASE)).unwrap().as_deref(), Some(BASE));
    }
}
