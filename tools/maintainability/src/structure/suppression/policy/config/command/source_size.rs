use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

#[cfg(test)]
use self::model::SOURCE_ROOT;
use self::model::{POLICY_PATH, ROOT_LOCKFILE, ROOT_MANIFEST, ToolingStructureManifest, is_source};

mod classification;
mod model;
mod profile;
mod revision;

const MAIN_SOURCE: &str = "tools/maintainability/src/main.rs";
const COMMAND_PROFILE_POLICY: &str = "policy/maintainability/reviewed-command-profiles.json";
const REPOSITORY_SENTINELS: &[(&str, &str)] = &[
    (ROOT_MANIFEST, "root manifest"),
    (ROOT_LOCKFILE, "root lockfile"),
    (MAIN_SOURCE, "executable source anchor"),
    (POLICY_PATH, "tooling structure policy"),
    (COMMAND_PROFILE_POLICY, "reviewed command profile policy"),
];

pub(super) fn validate_maintainability_analyzer(workspace: &Path, tracked_paths: &BTreeSet<String>, checked_paths: &BTreeSet<String>) -> Result<()> {
    require_repository_sentinels(workspace, tracked_paths)?;
    require_tracked_sources(tracked_paths, checked_paths)?;
    validate_inventory(workspace, checked_paths, checked_paths)
}

#[cfg(test)]
pub(super) fn validate_fixture(workspace: &Path, checked_paths: &BTreeSet<String>) -> Result<()> {
    if !checked_paths.iter().any(|path| is_source(path)) && !checked_paths.contains(ROOT_MANIFEST) {
        return Ok(());
    }
    require_repository_sentinels(workspace, checked_paths)?;
    validate_inventory(workspace, checked_paths, checked_paths)
}

fn require_repository_sentinels(workspace: &Path, tracked_paths: &BTreeSet<String>) -> Result<()> {
    for &(path, label) in REPOSITORY_SENTINELS {
        if !tracked_paths.contains(path) {
            bail!("maintainability analyzer {label} {path:?} must remain tracked");
        }
        require_regular_file(workspace, path, &format!("maintainability analyzer {label}"))?;
    }
    Ok(())
}

fn validate_inventory(workspace: &Path, checked_paths: &BTreeSet<String>, repository_paths: &BTreeSet<String>) -> Result<()> {
    let analyzer_sources = checked_paths.iter().filter(|path| is_source(path)).cloned().collect::<BTreeSet<_>>();
    let policy_bytes = fs::read(workspace.join(POLICY_PATH)).with_context(|| format!("read maintainability tooling structure policy {POLICY_PATH:?}"))?;
    let policy = ToolingStructureManifest::parse(&policy_bytes)?;
    let manifest_source = fs::read_to_string(workspace.join(ROOT_MANIFEST)).with_context(|| format!("read maintainability analyzer manifest {ROOT_MANIFEST:?}"))?;
    classification::validate_legacy_auto_target_inventory(workspace, &manifest_source, repository_paths)?;
    let mut observed = BTreeMap::new();
    let mut sources = BTreeMap::new();
    for path in &analyzer_sources {
        let source_path = workspace.join(path);
        let metadata = fs::symlink_metadata(&source_path).with_context(|| format!("inspect maintainability analyzer {path:?}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("maintainability analyzer source {path:?} must be a regular non-symlink file");
        }
        let source = fs::read_to_string(&source_path).with_context(|| format!("read maintainability analyzer {path:?}"))?;
        if has_bare_carriage_return(&source) {
            bail!("maintainability analyzer source {path:?} uses a bare carriage return; source lines must use LF or CRLF terminators");
        }
        observed.insert(path.clone(), physical_line_count(&source));
        sources.insert(path.clone(), source);
    }
    classification::validate_compiler_inputs(&sources, &manifest_source)?;
    let test_only = classification::test_only_paths(&sources, &manifest_source)?;
    let observed_profile = profile::fingerprint(workspace, &analyzer_sources)?;
    policy.compare_current(&observed, &observed_profile, &test_only)?;
    revision::compare_previous(workspace, &policy)
}

fn require_tracked_sources(tracked_paths: &BTreeSet<String>, checked_paths: &BTreeSet<String>) -> Result<()> {
    let tracked = tracked_paths.iter().filter(|path| is_source(path)).collect::<BTreeSet<_>>();
    let checked = checked_paths.iter().filter(|path| is_source(path)).collect::<BTreeSet<_>>();
    if tracked != checked {
        bail!("all maintainability analyzer Rust sources must be tracked: tracked={tracked:?}, checked={checked:?}");
    }
    Ok(())
}

fn require_regular_file(workspace: &Path, path: &str, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(workspace.join(path)).with_context(|| format!("inspect {label} {path:?}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} {path:?} must be a regular non-symlink file");
    }
    Ok(())
}

fn physical_line_count(source: &str) -> usize {
    source.bytes().filter(|byte| *byte == b'\n').count() + usize::from(!source.is_empty() && !source.ends_with('\n'))
}

fn has_bare_carriage_return(source: &str) -> bool {
    source
        .as_bytes()
        .iter()
        .enumerate()
        .any(|(index, byte)| *byte == b'\r' && source.as_bytes().get(index + 1) != Some(&b'\n'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_sentinels(workspace: &Path) -> BTreeSet<String> {
        for &(path, _) in REPOSITORY_SENTINELS {
            let target = workspace.join(path);
            fs::create_dir_all(target.parent().expect("sentinel parent")).expect("sentinel directory");
            fs::write(target, "sentinel\n").expect("sentinel file");
        }
        REPOSITORY_SENTINELS.iter().map(|(path, _)| (*path).to_owned()).collect()
    }

    #[test]
    fn every_repository_sentinel_must_be_tracked() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let complete = write_sentinels(workspace.path());
        for &(missing, _) in REPOSITORY_SENTINELS {
            let mut tracked = complete.clone();
            tracked.remove(missing);
            let error = require_repository_sentinels(workspace.path(), &tracked).unwrap_err();
            assert!(error.to_string().contains(missing), "{missing}: {error:#}");
            assert!(error.to_string().contains("must remain tracked"), "{missing}: {error:#}");
        }
    }

    #[test]
    fn untracked_physical_sentinels_do_not_satisfy_the_gate() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        write_sentinels(workspace.path());
        let error = require_repository_sentinels(workspace.path(), &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().contains(ROOT_MANIFEST), "{error:#}");
        assert!(error.to_string().contains("must remain tracked"), "{error:#}");
    }

    #[test]
    fn analyzer_lines_accept_lf_and_crlf_but_reject_bare_carriage_returns() {
        assert!(!has_bare_carriage_return("one\ntwo\n"));
        assert!(!has_bare_carriage_return("one\r\ntwo\r\n"));
        assert!(has_bare_carriage_return("one\rtwo\r"));
        assert_eq!(physical_line_count("one\ntwo\n"), 2);
        assert_eq!(physical_line_count("one\r\ntwo\r\n"), 2);
    }

    #[test]
    fn analyzer_sources_cannot_disable_the_guard_by_removing_its_manifest() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let target = workspace.path().join(MAIN_SOURCE);
        fs::create_dir_all(target.parent().expect("analyzer source parent")).expect("analyzer source directory");
        fs::write(target, "line\n").expect("analyzer source");

        let paths = BTreeSet::from([MAIN_SOURCE.to_owned()]);
        let error = validate_maintainability_analyzer(workspace.path(), &paths, &paths).unwrap_err();
        assert!(error.to_string().contains("root manifest"), "{error:#}");
        assert!(error.to_string().contains("must remain tracked"), "{error:#}");
        validate_fixture(workspace.path(), &BTreeSet::new()).expect("unrelated fixture without analyzer");
    }

    #[test]
    fn analyzer_inventory_requires_its_policy() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let mut without_policy = write_sentinels(workspace.path());
        without_policy.remove(POLICY_PATH);
        let source = format!("{SOURCE_ROOT}/small.rs");
        let target = workspace.path().join(&source);
        fs::create_dir_all(target.parent().expect("analyzer source parent")).expect("analyzer source directory");
        fs::write(target, "line\n").expect("analyzer source");
        without_policy.insert(source);

        let error = validate_maintainability_analyzer(workspace.path(), &without_policy, &without_policy).unwrap_err();
        assert!(error.to_string().contains("tooling structure policy"), "{error:#}");
    }

    #[test]
    fn untracked_analyzer_sources_are_rejected() {
        let tracked = BTreeSet::from([MAIN_SOURCE.to_owned()]);
        let checked = BTreeSet::from([MAIN_SOURCE.to_owned(), format!("{SOURCE_ROOT}/untracked.rs")]);
        let error = require_tracked_sources(&tracked, &checked).unwrap_err();
        assert!(error.to_string().contains("must be tracked"), "{error:#}");
    }

    #[test]
    fn analyzer_manifest_and_policy_must_be_regular_files() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let paths = BTreeSet::from([
            ROOT_MANIFEST.to_owned(),
            ROOT_LOCKFILE.to_owned(),
            MAIN_SOURCE.to_owned(),
            COMMAND_PROFILE_POLICY.to_owned(),
            POLICY_PATH.to_owned(),
        ]);
        for path in [ROOT_MANIFEST, ROOT_LOCKFILE, MAIN_SOURCE, COMMAND_PROFILE_POLICY] {
            let target = workspace.path().join(path);
            fs::create_dir_all(target.parent().expect("analyzer input parent")).expect("analyzer input directory");
            fs::write(target, "source\n").expect("analyzer input");
        }
        fs::create_dir_all(workspace.path().join(POLICY_PATH)).expect("policy directory");
        let error = validate_maintainability_analyzer(workspace.path(), &paths, &paths).unwrap_err();
        assert!(error.to_string().contains("regular non-symlink"), "{error:#}");

        fs::remove_dir(workspace.path().join(POLICY_PATH)).expect("remove policy directory");
        fs::write(workspace.path().join(POLICY_PATH), "{}").expect("policy file");
        fs::remove_file(workspace.path().join(ROOT_MANIFEST)).expect("remove root manifest");
        fs::create_dir(workspace.path().join(ROOT_MANIFEST)).expect("root manifest directory");
        let error = validate_maintainability_analyzer(workspace.path(), &paths, &paths).unwrap_err();
        assert!(error.to_string().contains("regular non-symlink"), "{error:#}");
    }

    #[cfg(unix)]
    #[test]
    fn analyzer_policy_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("temporary workspace");
        let paths = BTreeSet::from([
            ROOT_MANIFEST.to_owned(),
            ROOT_LOCKFILE.to_owned(),
            MAIN_SOURCE.to_owned(),
            COMMAND_PROFILE_POLICY.to_owned(),
            POLICY_PATH.to_owned(),
        ]);
        for path in [ROOT_MANIFEST, ROOT_LOCKFILE, MAIN_SOURCE, COMMAND_PROFILE_POLICY] {
            let target = workspace.path().join(path);
            fs::create_dir_all(target.parent().expect("analyzer input parent")).expect("analyzer input directory");
            fs::write(target, "source\n").expect("analyzer input");
        }
        let policy = workspace.path().join(POLICY_PATH);
        fs::create_dir_all(policy.parent().expect("policy parent")).expect("policy directory");
        fs::write(workspace.path().join("outside-policy.json"), "{}").expect("symlink target");
        symlink(workspace.path().join("outside-policy.json"), policy).expect("policy symlink");

        let error = validate_maintainability_analyzer(workspace.path(), &paths, &paths).unwrap_err();
        assert!(error.to_string().contains("regular non-symlink"), "{error:#}");
    }
}
