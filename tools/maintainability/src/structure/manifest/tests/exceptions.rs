use crate::structure::classify::Inventory;
use crate::structure::manifest::model::{FileExceptionKind, FileExceptionStatus, HotspotKind, HotspotStatus, StructureManifest};

use super::support::{file_exception, hotspot_manifest, inventory, ordinary_manifest};

#[test]
fn reviewed_file_exception_kinds_authorize_only_their_exact_classified_caps() {
    for (path, kind, previous_count, current_count, production) in [
        ("src/cohesive.rs", FileExceptionKind::ProductionCohesive, 799, 900, 700),
        ("tests/cohesive.rs", FileExceptionKind::TestCohesive, 1_000, 1_100, 0),
        ("tests/fixtures/history.rs", FileExceptionKind::HistoricalFixtureMatrix, 1_000, 1_400, 0),
    ] {
        let previous = ordinary_manifest(path, production, production);
        let previous_files = inventory(&[(path, previous_count, production)]);
        let mut current = previous.clone();
        current.file_exceptions.push(file_exception("reviewed.file-boundary", path, kind, current_count));
        let current_files = inventory(&[(path, current_count, production)]);

        compare(&current, &previous, &previous_files, &current_files).expect("reviewed exact exception passes");
    }
}

#[test]
fn file_exception_rejects_wrong_kind_excessive_cap_and_inexact_approval() {
    let previous = ordinary_manifest("src/cohesive.rs", 700, 700);
    let previous_files = inventory(&[("src/cohesive.rs", 799, 700)]);

    let mut wrong_kind = previous.clone();
    wrong_kind
        .file_exceptions
        .push(file_exception("reviewed.wrong-kind", "src/cohesive.rs", FileExceptionKind::TestCohesive, 900));
    let current_files = inventory(&[("src/cohesive.rs", 900, 700)]);
    assert!(wrong_kind.compare_current(&current_files).unwrap_err().to_string().contains("kind does not match"));

    let mut excessive = previous.clone();
    excessive
        .file_exceptions
        .push(file_exception("reviewed.excessive", "src/cohesive.rs", FileExceptionKind::ProductionCohesive, 1_001));
    assert!(excessive.validate_current().unwrap_err().to_string().contains("at most 1000"));

    let mut inexact = previous.clone();
    let mut exception = file_exception("reviewed.inexact", "src/cohesive.rs", FileExceptionKind::ProductionCohesive, 900);
    exception.approved_physical_ceiling = 950;
    inexact.file_exceptions.push(exception);
    assert!(
        inexact
            .compare_policy(&previous, &previous_files, &current_files)
            .unwrap_err()
            .to_string()
            .contains("exact observed physical lines")
    );
}

#[test]
fn each_exception_kind_enforces_its_own_hard_maximum() {
    for (path, kind, maximum) in [
        ("src/cohesive.rs", FileExceptionKind::ProductionCohesive, 1_000),
        ("tests/cohesive.rs", FileExceptionKind::TestCohesive, 1_200),
        ("tests/fixtures/history.rs", FileExceptionKind::HistoricalFixtureMatrix, 1_500),
    ] {
        let mut manifest = ordinary_manifest(path, 0, 0);
        manifest.file_exceptions.push(file_exception("reviewed.over-maximum", path, kind, maximum + 1));
        assert!(manifest.validate_current().unwrap_err().to_string().contains(&format!("at most {maximum}")));
    }
}

#[test]
fn active_exception_ceiling_ratchets_and_resolution_is_one_way() {
    let mut previous = ordinary_manifest("src/cohesive.rs", 700, 700);
    previous
        .file_exceptions
        .push(file_exception("reviewed.ratchet", "src/cohesive.rs", FileExceptionKind::ProductionCohesive, 900));
    let previous_files = inventory(&[("src/cohesive.rs", 900, 700)]);

    let mut reduced = previous.clone();
    reduced.file_exceptions[0].current_physical_ceiling = 850;
    let reduced_files = inventory(&[("src/cohesive.rs", 850, 700)]);
    compare(&reduced, &previous, &previous_files, &reduced_files).expect("active exception ratchets downward");
    assert!(
        reduced
            .compare_current(&inventory(&[("src/cohesive.rs", 849, 700)]))
            .unwrap_err()
            .to_string()
            .contains("must equal its observed physical lines")
    );

    let mut resolved = reduced.clone();
    resolved.file_exceptions[0].status = FileExceptionStatus::Resolved;
    resolved.file_exceptions[0].current_physical_ceiling = 800;
    let resolved_files = inventory(&[("src/cohesive.rs", 800, 700)]);
    compare(&resolved, &reduced, &reduced_files, &resolved_files).expect("ordinary limit resolves exception");

    let mut reactivated = resolved.clone();
    reactivated.file_exceptions[0].status = FileExceptionStatus::Active;
    reactivated.file_exceptions[0].current_physical_ceiling = 850;
    let reactivated_files = inventory(&[("src/cohesive.rs", 850, 700)]);
    assert!(
        reactivated
            .compare_policy(&resolved, &resolved_files, &reactivated_files)
            .unwrap_err()
            .to_string()
            .contains("resolved file exception")
    );

    let mut prematurely_resolved = reduced;
    prematurely_resolved.file_exceptions[0].status = FileExceptionStatus::Resolved;
    assert!(
        prematurely_resolved
            .compare_current(&reactivated_files)
            .unwrap_err()
            .to_string()
            .contains("exceeds its ordinary file limit")
    );
}

#[test]
fn exception_ledger_and_evidence_cannot_be_rewritten_or_duplicated() {
    let mut previous = ordinary_manifest("src/cohesive.rs", 700, 700);
    previous
        .file_exceptions
        .push(file_exception("reviewed.immutable", "src/cohesive.rs", FileExceptionKind::ProductionCohesive, 900));
    let files = inventory(&[("src/cohesive.rs", 900, 700)]);

    let mut removed = previous.clone();
    removed.file_exceptions.clear();
    assert!(removed.compare_policy(&previous, &files, &files).unwrap_err().to_string().contains("append-only"));

    let mut rewritten = previous.clone();
    rewritten.file_exceptions[0].owner = "new owner".to_owned();
    assert!(
        rewritten
            .compare_policy(&previous, &files, &files)
            .unwrap_err()
            .to_string()
            .contains("approval evidence is immutable")
    );

    let mut duplicate = previous.clone();
    duplicate
        .file_exceptions
        .push(file_exception("reviewed.second", "src/cohesive.rs", FileExceptionKind::ProductionCohesive, 900));
    assert!(duplicate.validate_current().unwrap_err().to_string().contains("more than one active"));

    let mut missing_evidence = previous;
    missing_evidence.file_exceptions[0].rationale.clear();
    assert!(missing_evidence.validate_current().unwrap_err().to_string().contains("rationale must not be empty"));
}

#[test]
fn historical_fixture_matrix_expires_at_its_due_phase() {
    let mut active = ordinary_manifest("tests/fixtures/history.rs", 0, 0);
    active.file_exceptions.push(file_exception(
        "fixture.published-history",
        "tests/fixtures/history.rs",
        FileExceptionKind::HistoricalFixtureMatrix,
        1_400,
    ));
    let active_files = inventory(&[("tests/fixtures/history.rs", 1_400, 0)]);
    active.validate_current().expect("fixture exception is active before its due phase");
    active.compare_current(&active_files).expect("fixture exception permits its exact ratcheted ceiling");

    let mut unnamed = active.clone();
    unnamed.file_exceptions[0].fixture_name = None;
    assert!(unnamed.validate_current().unwrap_err().to_string().contains("must have a name"));

    let mut overdue = active.clone();
    overdue.program_phase = 1;
    assert!(overdue.validate_current().unwrap_err().to_string().contains("due by phase 1"));

    let mut resolved = active.clone();
    resolved.program_phase = 1;
    resolved.file_exceptions[0].status = FileExceptionStatus::Resolved;
    resolved.file_exceptions[0].current_physical_ceiling = 1_000;
    let resolved_files = inventory(&[("tests/fixtures/history.rs", 1_000, 0)]);
    compare(&resolved, &active, &active_files, &resolved_files).expect("repayment permits the due phase to open");

    let mut skipped = ordinary_manifest("src/lib.rs", 1, 1);
    let phase_zero = skipped.clone();
    skipped.program_phase = 2;
    let ordinary_files = inventory(&[("src/lib.rs", 1, 1)]);
    assert!(
        skipped
            .compare_policy(&phase_zero, &ordinary_files, &ordinary_files)
            .unwrap_err()
            .to_string()
            .contains("one phase at a time")
    );

    let mut phase_one = phase_zero.clone();
    phase_one.program_phase = 1;
    assert!(
        phase_zero
            .compare_policy(&phase_one, &ordinary_files, &ordinary_files)
            .unwrap_err()
            .to_string()
            .contains("cannot move backward")
    );

    let mut schema_two = phase_zero;
    schema_two.schema_version = 2;
    assert!(
        phase_one
            .compare_policy(&schema_two, &ordinary_files, &ordinary_files)
            .unwrap_err()
            .to_string()
            .contains("must establish")
    );

    let mut renewed = resolved.clone();
    renewed.file_exceptions.push(file_exception(
        "fixture.renewed-history",
        "tests/fixtures/history.rs",
        FileExceptionKind::HistoricalFixtureMatrix,
        1_400,
    ));
    renewed.file_exceptions[1].removal_phase = Some(2);
    let renewed_files = inventory(&[("tests/fixtures/history.rs", 1_400, 0)]);
    assert!(
        renewed
            .compare_policy(&resolved, &resolved_files, &renewed_files)
            .unwrap_err()
            .to_string()
            .contains("cannot be renewed or transferred")
    );
}

#[test]
fn reviewed_file_limit_can_resolve_a_hotspot_without_erasing_aggregate_debt() {
    let mut resolved = hotspot_manifest(HotspotKind::Production, HotspotStatus::Resolved, &["src/hot.rs"], 900, 700);
    resolved
        .file_exceptions
        .push(file_exception("reviewed.hotspot-boundary", "src/hot.rs", FileExceptionKind::ProductionCohesive, 900));
    let files = inventory(&[("src/hot.rs", 900, 700)]);
    resolved.validate_current().expect("reviewed successor exception is valid");
    resolved.compare_current(&files).expect("reviewed file boundary resolves file-size debt");

    let mut active = resolved;
    active.hotspots[0].status = HotspotStatus::Active;
    assert!(active.compare_current(&files).unwrap_err().to_string().contains("must be marked resolved"));
}

fn compare(current: &StructureManifest, previous: &StructureManifest, previous_files: &Inventory, current_files: &Inventory) -> anyhow::Result<()> {
    previous.validate_previous()?;
    current.validate_current()?;
    previous.compare_current(previous_files)?;
    current.compare_current(current_files)?;
    current.compare_policy(previous, previous_files, current_files)
}
