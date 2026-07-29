use std::collections::BTreeSet;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};

use super::scope::validate_non_overlapping_subtrees;
use super::{
    CURRENT_SCHEMA_VERSION, ComponentVisibility, ExceptionDeltas, PHASE_ZERO_ISSUE, VisibilityException, VisibilityKind, VisibilityPolicy, VisibilityScope, budget_count,
    exception_deltas,
};

impl VisibilityPolicy {
    pub(super) fn validate(&self) -> Result<()> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            bail!("unsupported visibility policy schema {}", self.schema_version);
        }
        validate_revision(&self.baseline_commit)?;

        let components = validate_components(&self.components)?;
        let mut ids = BTreeSet::new();
        for exception in &self.exceptions {
            validate_name("exception ID", &exception.id)?;
            if !ids.insert(exception.id.as_str()) {
                bail!("duplicate visibility exception ID {:?}", exception.id);
            }
            if !components.contains(exception.component.as_str()) {
                bail!("visibility exception {:?} names unknown component {:?}", exception.id, exception.component);
            }
            if exception.delta == 0 {
                bail!("visibility exception {:?} delta must be positive", exception.id);
            }
            validate_exception_scope(exception)?;
            require_text(&exception.id, "owner", &exception.owner)?;
            validate_link(&exception.id, "issue", &exception.issue, "/issues/")?;
            validate_link(&exception.id, "pull request", &exception.pull_request, "/pull/")?;
            require_text(&exception.id, "rationale", &exception.rationale)?;
            require_text(&exception.id, "review phase", &exception.review_phase)?;
        }

        let deltas = exception_deltas(&self.exceptions)?;
        validate_non_overlapping_subtrees(&self.exceptions)?;
        for component in &self.components {
            validate_budget_ceiling(component, VisibilityKind::PubCrate, &deltas)?;
            validate_budget_ceiling(component, VisibilityKind::PubSuper, &deltas)?;
        }
        Ok(())
    }

    pub(super) fn validate_initial_policy(&self) -> Result<()> {
        if self.components.iter().any(|component| component.current != component.baseline) || !self.exceptions.is_empty() {
            bail!("initial visibility policy must preserve exact baseline counts and contain no exceptions");
        }
        Ok(())
    }
}

pub(super) fn validate_revision(revision: &str) -> Result<()> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("visibility baseline revision must be a full Git commit hash");
    }
    Ok(())
}

fn validate_components(components: &[ComponentVisibility]) -> Result<BTreeSet<&str>> {
    if components.is_empty() {
        bail!("visibility policy must enumerate every logical component");
    }
    let mut names = BTreeSet::new();
    let mut previous = None;
    for component in components {
        validate_name("component", &component.component)?;
        if previous.is_some_and(|previous| previous >= component.component.as_str()) {
            bail!("visibility policy components must be unique and sorted");
        }
        previous = Some(component.component.as_str());
        names.insert(component.component.as_str());
    }
    Ok(names)
}

fn validate_budget_ceiling(component: &ComponentVisibility, kind: VisibilityKind, deltas: &ExceptionDeltas<'_>) -> Result<()> {
    let baseline = budget_count(component.baseline, kind);
    let current = budget_count(component.current, kind);
    let delta = deltas.get(&(component.component.as_str(), kind)).copied().unwrap_or_default();
    let ceiling = baseline.checked_add(delta).context("visibility policy count overflow")?;
    if current > ceiling {
        bail!(
            "visibility current count for component {:?} and {kind:?} exceeds its baseline plus reviewed exceptions",
            component.component
        );
    }
    Ok(())
}

fn validate_exception_scope(exception: &VisibilityException) -> Result<()> {
    match exception.kind {
        VisibilityKind::PubCrate => {
            if exception.scope != VisibilityScope::CrossComponent || exception.subtree.is_some() {
                bail!("pub(crate) visibility exceptions require cross-component scope without a subtree");
            }
            if exception.issue == PHASE_ZERO_ISSUE {
                bail!("pub(crate) visibility exceptions require a distinct architectural issue, not the Phase 0 umbrella issue");
            }
        }
        VisibilityKind::PubSuper => {
            if exception.scope != VisibilityScope::ComponentSubtree {
                bail!("pub(super) visibility exceptions require component-subtree scope");
            }
            validate_subtree(exception.subtree.as_deref().context("pub(super) visibility exceptions require a subtree")?)?;
        }
    }
    Ok(())
}

fn validate_subtree(value: &str) -> Result<()> {
    let path = Path::new(value);
    if !value.starts_with("src/")
        || value.ends_with('/')
        || value.contains("//")
        || value.contains('\\')
        || path.is_absolute()
        || path.components().any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("visibility component subtree must be a normalized relative path under src/: {value:?}");
    }
    Ok(())
}

fn validate_name(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_'))
    {
        bail!("visibility {label} must use lowercase ASCII letters, digits, '.', '-', or '_'");
    }
    Ok(())
}

fn validate_link(id: &str, label: &str, value: &str, marker: &str) -> Result<()> {
    let prefix = "https://github.com/gearboxlogic/localhold";
    let Some(suffix) = value.strip_prefix(prefix).and_then(|value| value.strip_prefix(marker)) else {
        bail!("visibility exception {id:?} {label} must link to this repository");
    };
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("visibility exception {id:?} {label} must end in a numeric identifier");
    }
    Ok(())
}

fn require_text(id: &str, label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("visibility exception {id:?} {label} must not be empty");
    }
    Ok(())
}
