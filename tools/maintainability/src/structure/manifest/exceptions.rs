use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};

use super::measure::ObservedFiles;
use super::model::{
    FILE_EXCEPTION_SCHEMA_VERSION, FIXTURE_MATRIX_EXCEPTION_LIMIT, FileException, FileExceptionKind, FileExceptionStatus, PRODUCTION_EXCEPTION_LIMIT, PRODUCTION_FILE_LIMIT,
    StructureManifest, TEST_EXCEPTION_LIMIT, TEST_FILE_LIMIT,
};
use super::validate::{require_text, validate_id, validate_relative_rust_path};
use crate::structure::classify::FileMeasurement;

impl StructureManifest {
    pub(super) fn validate_file_exceptions(&self) -> Result<()> {
        let current_paths = self.current_path_map()?;
        let mut ids = BTreeSet::new();
        let mut active_paths = BTreeSet::new();
        let mut fixture_names = BTreeSet::new();
        for exception in &self.file_exceptions {
            validate_id(&exception.id, "file exception ID")?;
            if !ids.insert(exception.id.as_str()) {
                bail!("duplicate file exception ID {:?}", exception.id);
            }
            validate_relative_rust_path(&exception.path)?;
            validate_exception_contract(exception, self.program_phase)?;
            validate_exception_membership(exception, &current_paths, &mut active_paths)?;
            validate_fixture_name_uniqueness(exception, &mut fixture_names)?;
        }
        Ok(())
    }

    pub(super) fn compare_file_exceptions(&self, observed: &ObservedFiles<'_>) -> Result<()> {
        let active_paths: BTreeSet<_> = self
            .file_exceptions
            .iter()
            .filter(|exception| exception.status == FileExceptionStatus::Active)
            .map(|exception| exception.path.as_str())
            .collect();
        for exception in &self.file_exceptions {
            match exception.status {
                FileExceptionStatus::Active => compare_active_exception(exception, observed)?,
                FileExceptionStatus::Resolved => compare_resolved_exception(exception, observed, &active_paths)?,
            }
        }
        Ok(())
    }

    pub(super) fn compare_file_exception_policy(&self, previous: &Self, current_files: &ObservedFiles<'_>) -> Result<()> {
        self.compare_file_exceptions(current_files)?;
        if previous.schema_version < FILE_EXCEPTION_SCHEMA_VERSION && self.program_phase != 0 {
            bail!("structure schema version 3 must establish the maintainability program at phase 0");
        }
        compare_program_phase(previous.program_phase, self.program_phase)?;
        if self.file_exceptions.len() < previous.file_exceptions.len() {
            bail!("file exception ledger is append-only");
        }
        for (prior, current) in previous.file_exceptions.iter().zip(&self.file_exceptions) {
            compare_existing_exception(prior, current, current_files)?;
        }
        for exception in &self.file_exceptions[previous.file_exceptions.len()..] {
            compare_new_exception(exception, previous, self, current_files)?;
        }
        Ok(())
    }

    pub(super) fn applicable_file_limit(&self, file: &FileMeasurement) -> usize {
        self.file_exceptions
            .iter()
            .find(|exception| exception.status == FileExceptionStatus::Active && exception.path == file.path)
            .map_or_else(|| ordinary_file_limit(file), |exception| exception.current_physical_ceiling)
    }
}

fn validate_fixture_name_uniqueness<'a>(exception: &'a FileException, fixture_names: &mut BTreeSet<&'a str>) -> Result<()> {
    if exception.kind != FileExceptionKind::HistoricalFixtureMatrix {
        return Ok(());
    }
    let name = exception.fixture_name.as_deref().context("validated fixture exception lost its name")?;
    if !fixture_names.insert(name) {
        bail!("historical fixture matrix name {name:?} is reused");
    }
    Ok(())
}

fn validate_exception_membership<'a>(exception: &'a FileException, current_paths: &BTreeMap<&str, &str>, active_paths: &mut BTreeSet<&'a str>) -> Result<()> {
    if exception.status != FileExceptionStatus::Active {
        return Ok(());
    }
    if !current_paths.contains_key(exception.path.as_str()) {
        bail!("active file exception {:?} path is not in the current structural map", exception.id);
    }
    if !active_paths.insert(exception.path.as_str()) {
        bail!("path {:?} has more than one active file exception", exception.path);
    }
    Ok(())
}

fn compare_active_exception(exception: &FileException, observed: &ObservedFiles<'_>) -> Result<()> {
    let file = observed
        .get(exception.path.as_str())
        .with_context(|| format!("active file exception {:?} path is missing", exception.id))?;
    validate_exception_file_kind(exception, file)?;
    if file.physical_lines != exception.current_physical_ceiling {
        bail!(
            "file exception {:?} current ceiling must equal its observed physical lines: ceiling={}, observed={}",
            exception.id,
            exception.current_physical_ceiling,
            file.physical_lines
        );
    }
    Ok(())
}

fn compare_resolved_exception(exception: &FileException, observed: &ObservedFiles<'_>, active_paths: &BTreeSet<&str>) -> Result<()> {
    if active_paths.contains(exception.path.as_str()) {
        return Ok(());
    }
    let Some(file) = observed.get(exception.path.as_str()) else {
        return Ok(());
    };
    if file.physical_lines > ordinary_file_limit(file) {
        bail!("resolved file exception {:?} path exceeds its ordinary file limit", exception.id);
    }
    Ok(())
}

fn validate_exception_contract(exception: &FileException, program_phase: u32) -> Result<()> {
    let ordinary_limit = exception_ordinary_limit(exception.kind);
    let maximum = exception_maximum(exception.kind);
    if exception.approved_physical_ceiling <= ordinary_limit || exception.approved_physical_ceiling > maximum {
        bail!("file exception {:?} approved ceiling must be above {ordinary_limit} and at most {maximum}", exception.id);
    }
    if exception.current_physical_ceiling > exception.approved_physical_ceiling {
        bail!("file exception {:?} current ceiling cannot exceed its approved ceiling", exception.id);
    }
    if exception.status == FileExceptionStatus::Active && exception.current_physical_ceiling <= ordinary_limit {
        bail!("file exception {:?} is at or below its ordinary limit and must be resolved", exception.id);
    }
    require_text(&exception.id, "file exception", "owner", &exception.owner)?;
    require_text(&exception.id, "file exception", "issue", &exception.issue)?;
    require_text(&exception.id, "file exception", "pull request", &exception.pull_request)?;
    require_text(&exception.id, "file exception", "rationale", &exception.rationale)?;
    validate_exception_lifecycle(exception, program_phase)
}

fn validate_exception_lifecycle(exception: &FileException, program_phase: u32) -> Result<()> {
    match exception.kind {
        FileExceptionKind::HistoricalFixtureMatrix => {
            let name = exception.fixture_name.as_deref().context("historical fixture matrix exception must have a name")?;
            require_text(&exception.id, "file exception", "fixture name", name)?;
            let removal_phase = exception.removal_phase.context("historical fixture matrix exception must have a removal phase")?;
            if exception.status == FileExceptionStatus::Active && removal_phase <= program_phase {
                bail!(
                    "historical fixture matrix exception {:?} is due by phase {removal_phase} and must be resolved before phase {program_phase}",
                    exception.id
                );
            }
        }
        FileExceptionKind::ProductionCohesive | FileExceptionKind::TestCohesive => {
            if exception.fixture_name.is_some() || exception.removal_phase.is_some() {
                bail!("cohesive file exception {:?} cannot carry fixture-only lifecycle fields", exception.id);
            }
        }
    }
    Ok(())
}

fn compare_program_phase(previous: u32, current: u32) -> Result<()> {
    if current < previous {
        bail!("maintainability program phase cannot move backward from {previous} to {current}");
    }
    if current > previous.saturating_add(1) {
        bail!("maintainability program phase can advance only one phase at a time");
    }
    Ok(())
}

fn compare_existing_exception(previous: &FileException, current: &FileException, current_files: &ObservedFiles<'_>) -> Result<()> {
    if previous.id != current.id || !same_approval_identity(previous, current) {
        bail!("file exception ledger is append-only and approval evidence is immutable");
    }
    if previous.status == FileExceptionStatus::Resolved {
        if current != previous {
            bail!("resolved file exception {:?} is immutable", previous.id);
        }
        return Ok(());
    }
    if current.current_physical_ceiling > previous.current_physical_ceiling {
        bail!("file exception {:?} current ceiling cannot increase", current.id);
    }
    if current.status == FileExceptionStatus::Resolved {
        match current_files.get(current.path.as_str()) {
            Some(file) if current.current_physical_ceiling != file.physical_lines => {
                bail!("resolved file exception {:?} must record its final observed physical lines", current.id);
            }
            None if current.current_physical_ceiling != previous.current_physical_ceiling => {
                bail!("removed file exception {:?} must preserve its last measured ceiling", current.id);
            }
            _ => {}
        }
    }
    Ok(())
}

fn compare_new_exception(exception: &FileException, previous: &StructureManifest, current: &StructureManifest, current_files: &ObservedFiles<'_>) -> Result<()> {
    if exception.status != FileExceptionStatus::Active {
        bail!("new file exception {:?} must start active", exception.id);
    }
    if conflicts_with_historical_fixture_approval(exception, previous, current) {
        bail!("historical fixture matrix exception {:?} cannot be renewed or transferred", exception.id);
    }
    let file = current_files
        .get(exception.path.as_str())
        .with_context(|| format!("new file exception {:?} path is missing", exception.id))?;
    validate_exception_file_kind(exception, file)?;
    if exception.approved_physical_ceiling != file.physical_lines || exception.current_physical_ceiling != file.physical_lines {
        bail!(
            "new file exception {:?} must set approved and current ceilings to its exact observed physical lines",
            exception.id
        );
    }
    Ok(())
}

fn conflicts_with_historical_fixture_approval(exception: &FileException, previous: &StructureManifest, current: &StructureManifest) -> bool {
    let historical = previous
        .file_exceptions
        .iter()
        .filter(|prior| prior.kind == FileExceptionKind::HistoricalFixtureMatrix)
        .collect::<Vec<_>>();
    if historical
        .iter()
        .any(|prior| exception.kind == FileExceptionKind::HistoricalFixtureMatrix && prior.fixture_name == exception.fixture_name)
    {
        return true;
    }
    let mut paths = historical.iter().map(|prior| prior.path.as_str()).collect::<BTreeSet<_>>();
    loop {
        let before = paths.len();
        for evolution in &current.path_evolutions {
            if evolution.sources.iter().any(|source| paths.contains(source.as_str())) {
                paths.extend(evolution.successors.iter().map(String::as_str));
            }
        }
        if paths.len() == before {
            return paths.contains(exception.path.as_str());
        }
    }
}

fn validate_exception_file_kind(exception: &FileException, file: &FileMeasurement) -> Result<()> {
    let matches = match exception.kind {
        FileExceptionKind::ProductionCohesive => file.production_lines > 0,
        FileExceptionKind::TestCohesive | FileExceptionKind::HistoricalFixtureMatrix => file.production_lines == 0,
    };
    if !matches {
        bail!("file exception {:?} kind does not match the classified file", exception.id);
    }
    Ok(())
}

fn same_approval_identity(previous: &FileException, current: &FileException) -> bool {
    previous.path == current.path
        && previous.kind == current.kind
        && previous.approved_physical_ceiling == current.approved_physical_ceiling
        && previous.owner == current.owner
        && previous.issue == current.issue
        && previous.pull_request == current.pull_request
        && previous.rationale == current.rationale
        && previous.fixture_name == current.fixture_name
        && previous.removal_phase == current.removal_phase
}

const fn exception_ordinary_limit(kind: FileExceptionKind) -> usize {
    match kind {
        FileExceptionKind::ProductionCohesive => PRODUCTION_FILE_LIMIT,
        FileExceptionKind::TestCohesive | FileExceptionKind::HistoricalFixtureMatrix => TEST_FILE_LIMIT,
    }
}

const fn exception_maximum(kind: FileExceptionKind) -> usize {
    match kind {
        FileExceptionKind::ProductionCohesive => PRODUCTION_EXCEPTION_LIMIT,
        FileExceptionKind::TestCohesive => TEST_EXCEPTION_LIMIT,
        FileExceptionKind::HistoricalFixtureMatrix => FIXTURE_MATRIX_EXCEPTION_LIMIT,
    }
}

pub(super) const fn ordinary_file_limit(file: &FileMeasurement) -> usize {
    if file.production_lines == 0 { TEST_FILE_LIMIT } else { PRODUCTION_FILE_LIMIT }
}

pub(super) const fn file_kind(file: &FileMeasurement) -> &'static str {
    if file.production_lines == 0 { "test" } else { "production" }
}
