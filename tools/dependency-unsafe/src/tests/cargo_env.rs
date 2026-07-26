use std::ffi::OsStr;
use std::fs;

use sha2::{Digest, Sha256};
use tempfile::tempdir;

use super::{CargoEnvironment, reject_cargo_config, source_configuration_variable, temporary_root};

const SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";

fn lockfile(name: &str, version: &str, checksum: &str, source: &str) -> String {
    format!("version = 4\n\n[[package]]\nname = {name:?}\nversion = {version:?}\nsource = {source:?}\nchecksum = {checksum:?}\n")
}

fn fixture_home(archive: &[u8]) -> tempfile::TempDir {
    let home = tempdir().expect("temporary Cargo home");
    let cache = home.path().join("registry/cache/index-key");
    let index = home.path().join("registry/index/index-key");
    fs::create_dir_all(&cache).expect("create registry cache");
    fs::create_dir_all(&index).expect("create registry index");
    fs::write(cache.join("fixture-1.2.3.crate"), archive).expect("write registry archive");
    fs::write(index.join("config.json"), "{}").expect("write registry index");
    home
}

#[test]
fn isolated_home_copies_only_digest_verified_archives() {
    let archive = b"crate archive bytes";
    let checksum = format!("{:x}", Sha256::digest(archive));
    let source_home = fixture_home(archive);
    let workspace = tempdir().expect("temporary workspace");
    let environment = CargoEnvironment::prepare_from(&lockfile("fixture", "1.2.3", &checksum, SOURCE), source_home.path(), workspace.path()).expect("prepare isolated Cargo home");

    let copied = environment.home_path.join("registry/cache/index-key/fixture-1.2.3.crate");
    assert_eq!(fs::read(copied).expect("read copied archive"), archive);
    assert!(environment.home_path.join("registry/index/index-key/config.json").is_file());

    let output = environment
        .cargo_command()
        .expect("build pinned Cargo command")
        .arg("--version")
        .output()
        .expect("run pinned Cargo outside the workspace");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn altered_missing_and_conflicting_duplicate_archives_are_rejected() {
    let archive = b"crate archive bytes";
    let checksum = format!("{:x}", Sha256::digest(archive));
    let source_home = fixture_home(archive);
    let workspace = tempdir().expect("temporary workspace");
    assert!(CargoEnvironment::prepare_from(&lockfile("fixture", "1.2.3", &"0".repeat(64), SOURCE), source_home.path(), workspace.path()).is_err());

    fs::remove_file(source_home.path().join("registry/cache/index-key/fixture-1.2.3.crate")).expect("remove archive");
    assert!(CargoEnvironment::prepare_from(&lockfile("fixture", "1.2.3", &checksum, SOURCE), source_home.path(), workspace.path()).is_err());

    let source_home = fixture_home(archive);
    let second = source_home.path().join("registry/cache/other-index");
    fs::create_dir(&second).expect("create second cache");
    fs::write(second.join("fixture-1.2.3.crate"), archive).expect("write identical duplicate archive");
    CargoEnvironment::prepare_from(&lockfile("fixture", "1.2.3", &checksum, SOURCE), source_home.path(), workspace.path())
        .expect("identical verified cache entries are equivalent");

    fs::write(second.join("fixture-1.2.3.crate"), b"tampered duplicate").expect("tamper duplicate archive");
    assert!(CargoEnvironment::prepare_from(&lockfile("fixture", "1.2.3", &checksum, SOURCE), source_home.path(), workspace.path()).is_err());
}

#[test]
fn cargo_config_in_any_working_directory_ancestor_is_rejected() {
    let root = tempdir().expect("temporary root");
    let cwd = root.path().join("nested/cwd");
    let cargo_home = root.path().join("isolated-home");
    fs::create_dir_all(&cwd).expect("create working directory");
    fs::create_dir(&cargo_home).expect("create Cargo home");
    fs::create_dir(root.path().join(".cargo")).expect("create ancestor Cargo configuration directory");
    fs::write(root.path().join(".cargo/config.toml"), "[source.crates-io]\nreplace-with = \"untrusted\"\n").expect("write ancestor source replacement");

    assert!(reject_cargo_config(&cwd, &cargo_home).is_err());
}

#[test]
fn isolated_cargo_home_config_is_rejected() {
    let archive = b"crate archive bytes";
    let checksum = format!("{:x}", Sha256::digest(archive));
    let source_home = fixture_home(archive);
    let workspace = tempdir().expect("temporary workspace");
    let environment =
        CargoEnvironment::prepare_from(&lockfile("fixture", "1.2.3", &checksum, SOURCE), source_home.path(), workspace.path()).expect("prepare isolated Cargo environment");
    fs::write(environment.home_path.join("config.toml"), "[source.crates-io]\nreplace-with = \"untrusted\"\n").expect("write isolated-home source replacement");

    assert!(environment.cargo_command().is_err());
}

#[cfg(unix)]
#[test]
fn physical_ancestor_config_is_rejected_through_symlinked_temp_root() {
    use std::os::unix::fs::symlink;

    let physical = tempdir().expect("physical temporary root");
    fs::create_dir(physical.path().join(".cargo")).expect("create physical Cargo configuration directory");
    fs::write(physical.path().join(".cargo/config.toml"), "[source.crates-io]\nreplace-with = \"untrusted\"\n").expect("write physical source replacement");
    fs::create_dir(physical.path().join("temp")).expect("create physical temp directory");

    let lexical = tempdir().expect("lexical temporary root");
    symlink(physical.path().join("temp"), lexical.path().join("linked-temp")).expect("create temp-root symlink");
    let archive = b"crate archive bytes";
    let checksum = format!("{:x}", Sha256::digest(archive));
    let source_home = fixture_home(archive);
    let workspace = tempdir().expect("temporary workspace");

    assert!(
        CargoEnvironment::prepare_from_with_cwd_root(
            &lockfile("fixture", "1.2.3", &checksum, SOURCE),
            source_home.path(),
            workspace.path(),
            Some(&lexical.path().join("linked-temp")),
        )
        .is_err()
    );
}

#[test]
fn source_configuration_environment_names_are_removed_case_insensitively() {
    assert!(source_configuration_variable(OsStr::new("CARGO_SOURCE_CRATES_IO_REPLACE_WITH")));
    assert!(source_configuration_variable(OsStr::new("cargo_registries_crates_io_index")));
    assert!(!source_configuration_variable(OsStr::new("CARGO_TERM_COLOR")));
}

#[test]
fn unsupported_registry_sources_are_rejected() {
    let source_home = fixture_home(b"crate archive bytes");
    let workspace = tempdir().expect("temporary workspace");
    let lockfile = lockfile("fixture", "1.2.3", &"0".repeat(64), "git+https://example.invalid/repository");
    assert!(CargoEnvironment::prepare_from(&lockfile, source_home.path(), workspace.path()).is_err());
}

#[cfg(unix)]
#[test]
fn symlinked_archives_and_index_entries_are_rejected() {
    use std::os::unix::fs::symlink;

    let archive = b"crate archive bytes";
    let checksum = format!("{:x}", Sha256::digest(archive));
    let source_home = fixture_home(archive);
    let workspace = tempdir().expect("temporary workspace");
    let archive_path = source_home.path().join("registry/cache/index-key/fixture-1.2.3.crate");
    fs::remove_file(&archive_path).expect("remove regular archive");
    symlink(source_home.path().join("outside"), &archive_path).expect("create archive symlink");
    assert!(CargoEnvironment::prepare_from(&lockfile("fixture", "1.2.3", &checksum, SOURCE), source_home.path(), workspace.path()).is_err());

    let source_home = fixture_home(archive);
    symlink(
        source_home.path().join("registry/index/index-key/config.json"),
        source_home.path().join("registry/index/index-key/alias"),
    )
    .expect("create index symlink");
    assert!(CargoEnvironment::prepare_from(&lockfile("fixture", "1.2.3", &checksum, SOURCE), source_home.path(), workspace.path()).is_err());
}

#[cfg(unix)]
#[test]
fn symlinked_audit_temporary_root_is_rejected() {
    use std::os::unix::fs::symlink;

    let workspace = tempdir().expect("temporary workspace");
    let outside = tempdir().expect("outside directory");
    symlink(outside.path(), workspace.path().join(".cache")).expect("create temporary-root symlink");
    assert!(temporary_root(workspace.path()).is_err());
    assert!(fs::read_dir(outside.path()).expect("read outside directory").next().is_none());
}
