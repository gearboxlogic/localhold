use serde::Deserialize;

use super::super::SourceCategory;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Policy {
    pub(super) schema_version: u32,
    pub(super) adoption_commit: String,
    pub(super) source_baselines: Vec<BaselineReference>,
    pub(super) source_governance: Vec<SourceGovernance>,
    pub(super) source_exceptions: Vec<SourceException>,
    pub(super) cargo_allowances_paths: Vec<String>,
    pub(super) clippy_configuration_path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct BaselineReference {
    pub(super) category: SourceCategory,
    pub(super) path: String,
    pub(super) expected_sites: usize,
    pub(super) expected_signatures: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceGovernance {
    pub(super) category: SourceCategory,
    pub(super) owner: String,
    pub(super) issue: String,
    pub(super) rationale: String,
    pub(super) safety_invariant: String,
    pub(super) alternatives_considered: String,
    pub(super) evidence: String,
    pub(super) re_review_phase: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceException {
    pub(super) id: String,
    pub(super) source_id: String,
    pub(super) category: SourceCategory,
    pub(super) max_occurrences: usize,
    pub(super) disposition: Disposition,
    pub(super) status: Status,
    pub(super) owner: String,
    pub(super) issue: String,
    pub(super) pull_request: String,
    pub(super) rationale: String,
    pub(super) safety_invariant: String,
    pub(super) alternatives_considered: String,
    pub(super) evidence: String,
    pub(super) re_review_phase: String,
    pub(super) removal_issue: Option<String>,
    pub(super) removal_phase: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceBaseline {
    pub(super) schema_version: u32,
    pub(super) category: SourceCategory,
    pub(super) entries: Vec<SourceBaselineEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceBaselineEntry {
    pub(super) id: String,
    pub(super) max_occurrences: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct CargoAllowanceFile {
    pub(super) schema_version: u32,
    pub(super) entries: Vec<CargoAllowance>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct CargoAllowance {
    pub(super) id: String,
    pub(super) manifest: String,
    pub(super) family: String,
    pub(super) lint: String,
    pub(super) priority: i64,
    pub(super) disposition: Disposition,
    pub(super) status: Status,
    pub(super) owner: String,
    pub(super) issue: String,
    pub(super) pull_request: String,
    pub(super) rationale: String,
    pub(super) safety_invariant: String,
    pub(super) alternatives_considered: String,
    pub(super) substitute: String,
    pub(super) sentinel: String,
    pub(super) evidence: String,
    pub(super) re_review_phase: String,
    pub(super) removal_issue: Option<String>,
    pub(super) removal_phase: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct ClippyConfigurationFile {
    pub(super) schema_version: u32,
    pub(super) entries: Vec<ClippySetting>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct ClippySetting {
    pub(super) id: String,
    pub(super) key: String,
    pub(super) constraint: ClippyConstraint,
    pub(super) owner: String,
    pub(super) issue: String,
    pub(super) pull_request: String,
    pub(super) rationale: String,
    pub(super) safety_invariant: String,
    pub(super) alternatives_considered: String,
    pub(super) sentinel: String,
    pub(super) evidence: String,
    pub(super) re_review_phase: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum ClippyConstraint {
    MaximumInteger { value: i64 },
    StringSubset { values: Vec<String> },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Disposition {
    Permanent,
    Temporary,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Status {
    Active,
    Retired,
}
