use serde::Deserialize;

pub(super) const CURRENT_SCHEMA_VERSION: u32 = 4;
pub(super) const FILE_EXCEPTION_SCHEMA_VERSION: u32 = 3;
pub(super) const EVOLUTION_SCHEMA_VERSION: u32 = 2;
pub(super) const LEGACY_SCHEMA_VERSION: u32 = 1;
pub(super) const PRODUCTION_FILE_LIMIT: usize = 800;
pub(super) const TEST_FILE_LIMIT: usize = 1_000;
pub(super) const PRODUCTION_EXCEPTION_LIMIT: usize = 1_000;
pub(super) const TEST_EXCEPTION_LIMIT: usize = 1_200;
pub(super) const FIXTURE_MATRIX_EXCEPTION_LIMIT: usize = 1_500;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StructureManifest {
    pub(super) schema_version: u32,
    #[serde(default)]
    pub(super) program_phase: u32,
    #[serde(default)]
    pub(super) feature_freeze: FeatureFreezeStatus,
    pub baseline_commit: String,
    pub tracked_roots: Vec<String>,
    pub(super) limits: Limits,
    pub(super) pre_gate_adjustments: Vec<PreGateAdjustment>,
    pub(super) components: Vec<LogicalComponent>,
    pub(super) hotspots: Vec<Hotspot>,
    #[serde(default)]
    pub(super) path_evolutions: Vec<PathEvolution>,
    #[serde(default)]
    pub(super) component_transfers: Vec<ComponentTransfer>,
    #[serde(default)]
    pub(super) file_exceptions: Vec<FileException>,
    #[serde(default)]
    pub(super) split_allowances: Vec<SplitAllowance>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum FeatureFreezeStatus {
    #[default]
    Active,
    Exited,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Limits {
    pub(super) production_file_physical_lines: usize,
    pub(super) test_file_physical_lines: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct PreGateAdjustment {
    pub(super) id: String,
    pub(super) component: String,
    pub(super) hotspot: String,
    pub(super) physical_lines: usize,
    pub(super) production_lines: usize,
    pub(super) issue: String,
    pub(super) pull_request: String,
    pub(super) rationale: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct LogicalComponent {
    pub(super) id: String,
    pub(super) baseline_paths: Vec<String>,
    pub(super) paths: Vec<String>,
    pub(super) baseline_production_lines: usize,
    pub(super) production_ceiling: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum HotspotKind {
    Production,
    Test,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum HotspotStatus {
    Active,
    Resolved,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Hotspot {
    pub(super) id: String,
    pub(super) component: String,
    pub(super) kind: HotspotKind,
    pub(super) status: HotspotStatus,
    pub(super) baseline_path: String,
    pub(super) baseline_physical_lines: usize,
    pub(super) baseline_production_lines: usize,
    pub(super) successors: Vec<String>,
    pub(super) physical_ceiling: usize,
    pub(super) production_ceiling: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum PathEvolutionKind {
    Rename,
    Split,
    TestExtraction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct PathEvolution {
    pub(super) id: String,
    pub(super) kind: PathEvolutionKind,
    pub(super) sources: Vec<String>,
    pub(super) successors: Vec<String>,
    pub(super) issue: String,
    pub(super) pull_request: String,
    pub(super) rationale: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct ComponentTransfer {
    pub(super) id: String,
    pub(super) source_component: String,
    pub(super) destination_component: String,
    pub(super) production_lines: usize,
    pub(super) paths: Vec<String>,
    pub(super) path_evolution: String,
    pub(super) issue: String,
    pub(super) pull_request: String,
    pub(super) rationale: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum FileExceptionKind {
    ProductionCohesive,
    TestCohesive,
    HistoricalFixtureMatrix,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum FileExceptionStatus {
    Active,
    Resolved,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct FileException {
    pub(super) id: String,
    pub(super) path: String,
    pub(super) kind: FileExceptionKind,
    pub(super) status: FileExceptionStatus,
    pub(super) approved_physical_ceiling: usize,
    pub(super) current_physical_ceiling: usize,
    pub(super) owner: String,
    pub(super) issue: String,
    pub(super) pull_request: String,
    pub(super) rationale: String,
    pub(super) fixture_name: Option<String>,
    pub(super) removal_phase: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum SplitAllowanceStatus {
    Active,
    Resolved,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct SplitAllowance {
    pub(super) id: String,
    pub(super) hotspot: String,
    pub(super) path_evolution: String,
    pub(super) status: SplitAllowanceStatus,
    pub(super) approved_physical_lines: usize,
    pub(super) approved_production_lines: usize,
    pub(super) current_physical_lines: usize,
    pub(super) current_production_lines: usize,
    pub(super) owner: String,
    pub(super) recovery_issue: String,
    pub(super) pull_request: String,
    pub(super) rationale: String,
    pub(super) due_phase: u32,
}
