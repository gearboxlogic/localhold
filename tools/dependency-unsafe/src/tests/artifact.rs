use std::collections::BTreeMap;
use std::fs;

use tempfile::tempdir;

use super::{GeneratedArtifacts, read_artifacts};

#[test]
fn artifact_write_is_exact_and_lf_terminated() {
    let workspace = tempdir().expect("temporary workspace");
    let directory = workspace.path().join("artifact");
    fs::create_dir(&directory).expect("create artifact directory");
    fs::write(directory.join("stale.json"), "{}").expect("write stale artifact");
    let artifacts = GeneratedArtifacts {
        files: BTreeMap::from([
            ("a.jsonl".to_owned(), b"{\"record\":\"a\"}\n".to_vec()),
            ("manifest.json".to_owned(), b"{\"schema_version\":1}\n".to_vec()),
        ]),
    };

    artifacts.write(workspace.path(), &directory).expect("write artifacts");
    assert!(!directory.join("stale.json").exists());
    assert_eq!(read_artifacts(workspace.path(), &directory).expect("read exact artifact set"), artifacts.files);
    assert_eq!(fs::read(directory.join("a.jsonl")).expect("read JSONL"), b"{\"record\":\"a\"}\n");
}

#[test]
fn artifact_write_rejects_unexpected_entries_without_modifying_directory() {
    let workspace = tempdir().expect("temporary workspace");
    let directory = workspace.path().join("artifact");
    fs::create_dir(&directory).expect("create artifact directory");
    fs::write(directory.join("operator-note.txt"), "do not discard").expect("write unexpected file");
    let artifacts = GeneratedArtifacts {
        files: BTreeMap::from([("manifest.json".to_owned(), b"{}\n".to_vec())]),
    };
    assert!(artifacts.write(workspace.path(), &directory).is_err());
    assert_eq!(fs::read_to_string(directory.join("operator-note.txt")).expect("preserved file"), "do not discard");
}

#[test]
fn missing_or_stale_baseline_writes_regenerated_evidence() {
    let directory = tempdir().expect("temporary directory");
    let baseline = directory.path().join("baseline");
    let actual = directory.path().join("actual");
    let artifacts = GeneratedArtifacts {
        files: BTreeMap::from([("manifest.json".to_owned(), b"{\"schema_version\":1}\n".to_vec())]),
    };

    assert!(artifacts.check(directory.path(), &baseline, &actual).is_err());
    assert_eq!(fs::read(actual.join("manifest.json")).expect("read actual evidence"), b"{\"schema_version\":1}\n");

    fs::create_dir(&baseline).expect("create baseline");
    fs::write(baseline.join("manifest.json"), "{\"schema_version\":0}\n").expect("write stale baseline");
    assert!(artifacts.check(directory.path(), &baseline, &actual).is_err());

    artifacts.write(directory.path(), &baseline).expect("write matching baseline");
    artifacts.check(directory.path(), &baseline, &actual).expect("matching baseline");
}

#[cfg(unix)]
#[test]
fn artifact_write_rejects_symlinked_output_ancestors_without_outside_changes() {
    use std::os::unix::fs::symlink;

    let workspace = tempdir().expect("temporary workspace");
    let outside = tempdir().expect("outside directory");
    symlink(outside.path(), workspace.path().join("target")).expect("create output ancestor symlink");
    let artifacts = GeneratedArtifacts {
        files: BTreeMap::from([("manifest.json".to_owned(), b"{}\n".to_vec())]),
    };

    let destination = workspace.path().join("target/dependency-unsafe/actual-linux");
    assert!(artifacts.write(workspace.path(), &destination).is_err());
    assert!(fs::read_dir(outside.path()).expect("read outside directory").next().is_none());
}

#[cfg(unix)]
#[test]
fn artifact_read_and_write_reject_root_and_leaf_symlinks() {
    use std::os::unix::fs::symlink;

    let workspace = tempdir().expect("temporary workspace");
    let outside = tempdir().expect("outside directory");
    fs::write(outside.path().join("manifest.json"), "{}\n").expect("write outside artifact");
    let root_link = workspace.path().join("baseline");
    symlink(outside.path(), &root_link).expect("create artifact root symlink");
    assert!(read_artifacts(workspace.path(), &root_link).is_err());

    let actual = workspace.path().join("actual");
    fs::create_dir(&actual).expect("create actual directory");
    symlink(outside.path().join("manifest.json"), actual.join("manifest.json")).expect("create artifact leaf symlink");
    let artifacts = GeneratedArtifacts {
        files: BTreeMap::from([("manifest.json".to_owned(), b"{\"new\":true}\n".to_vec())]),
    };
    assert!(artifacts.write(workspace.path(), &actual).is_err());
    assert_eq!(fs::read_to_string(outside.path().join("manifest.json")).expect("outside artifact unchanged"), "{}\n");
}
