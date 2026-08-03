use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::action_inputs;

pub(super) const DEPENDENCY_REVIEW_ACTION: &str = "actions/dependency-review-action@a1d282b36b6f3519aa1f3fc636f609c47dddb294";
pub(super) const MISE_ACTION: &str = "jdx/mise-action@e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d";

const DEPENDENCY_REVIEW_PATH: &str = ".github/workflows/dependency-review.yml";
const PR_CLASSIFICATION_PATH: &str = ".github/workflows/pr-classification.yml";
const PR_CLASSIFICATION_PACKAGE_PREFIX: &str = "script/pr_classification/";
const PR_CLASSIFICATION_MODULE_ALIAS: &str = "script/pr_classification.py";
const ORIGINAL_PR_CLASSIFICATION_PROFILE: &[ReviewedFile] = &[
    ReviewedFile::new(PR_CLASSIFICATION_PATH, "25c11bb0bd514363df2a539d9ceb88d8c954b0c92ec0d9d9e70b9bde32f8b6dd"),
    ReviewedFile::new(
        "policy/maintainability/feature-freeze.json",
        "50119661e8bfead163fe4051784505777ae80daef33c5578de0395563e9997e8",
    ),
    ReviewedFile::new("script/check_pr_classification.py", "64f498229401c518ee377b5a74ec9f9c4c946b424316b49e979d5155469720e2"),
    ReviewedFile::new("script/pr_classification/__init__.py", "a8ee1ff16a8e133d6c930231522ca7803b69d3e81b2f9b7ad43b8841a89b3705"),
    ReviewedFile::new("script/pr_classification/github_api.py", "f6bb2b19274b6c207dafba9dd18cb8eb1611dcde9f2f9ac328f3a0de3c4c76c5"),
    ReviewedFile::new("script/pr_classification/markdown.py", "ecfc33f63804491d99bfce35dc440bade7bf84bb9cca68753e1dfa865a99b822"),
    ReviewedFile::new("script/pr_classification/model.py", "89e9ad2aca63bf5521cf7b31247f1ac7cceb53d20cff888799f042ef57cb66fe"),
    ReviewedFile::new("script/pr_classification/policy.py", "c0e816e57ad638b42a92bcb67f40169a7d6fc16b09e214af731483a64b61218e"),
    ReviewedFile::new("script/pr_classification/reviews.py", "ff9687dab05e4c5244af0a074cf21d83d2301b44ac406aa80da94ac8e9960bd5"),
    ReviewedFile::new("script/pr_classification/validation.py", "d9ddf69a1378b3d7d27e9b4dc9b9968d88b44bf713bcb22cda6ce6aed35bec7b"),
];
const HARDENED_PR_CLASSIFICATION_PROFILE: &[ReviewedFile] = &[
    ReviewedFile::new(PR_CLASSIFICATION_PATH, "25c11bb0bd514363df2a539d9ceb88d8c954b0c92ec0d9d9e70b9bde32f8b6dd"),
    ReviewedFile::new(
        "policy/maintainability/feature-freeze.json",
        "13086fe757b5613bd3faec4b4f5228df6d3413da8c6474452e6a604621340048",
    ),
    ReviewedFile::new("script/check_pr_classification.py", "64f498229401c518ee377b5a74ec9f9c4c946b424316b49e979d5155469720e2"),
    ReviewedFile::new("script/pr_classification/__init__.py", "a8ee1ff16a8e133d6c930231522ca7803b69d3e81b2f9b7ad43b8841a89b3705"),
    ReviewedFile::new("script/pr_classification/github_api.py", "f6bb2b19274b6c207dafba9dd18cb8eb1611dcde9f2f9ac328f3a0de3c4c76c5"),
    ReviewedFile::new("script/pr_classification/markdown.py", "ecfc33f63804491d99bfce35dc440bade7bf84bb9cca68753e1dfa865a99b822"),
    ReviewedFile::new("script/pr_classification/model.py", "822d5b19e91a6691ebb26249d65d3e6381a6e016766a3c6edfe07cdff83b2d82"),
    ReviewedFile::new("script/pr_classification/policy.py", "6fe2900394d66e25c78bfa16de90c93275abeab5f179ba9cbf33570e89dec230"),
    ReviewedFile::new("script/pr_classification/reviews.py", "d26ec855a798f5b3df7ab205c620cf8bb4bb69429c150d437cf89567a4bcab19"),
    ReviewedFile::new("script/pr_classification/validation.py", "ff679b94f3eec0c9464166e4d160aa4f4a2a9950695d55acb2dcb5e226c9c6fc"),
];
const PR_CLASSIFICATION_PROFILES: &[&[ReviewedFile]] = &[ORIGINAL_PR_CLASSIFICATION_PROFILE, HARDENED_PR_CLASSIFICATION_PROFILE];
const MISE_VERSION: &str = "2026.7.5";
const REVIEWED_MISE_PROFILES: &[(&str, &str)] = &[(
    "627903d61cd155a318e0dffa4a29052099fbed1834bd485e7859fdcad03c0529",
    "24a3c64cbd2123ba9ab457eba21a65c7960d189d6685fe1d2bfd4a979134c358",
)];
// Stage intentional changes by adding the next complete profile beside the
// current one, merging that checker ratchet, and only then changing the
// guarded files. Remove the obsolete profile in a final cleanup change.
const REVIEWED_DEPENDENCY_REVIEW_WORKFLOWS: &[&str] = &[r"name: Dependency Review

on:
  pull_request:
    branches: [main]

permissions:
  contents: read

jobs:
  dependency-review:
    name: Dependency Review
    if: ${{ !github.event.repository.private }}
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7.0.0
        with:
          persist-credentials: false

      - uses: actions/dependency-review-action@a1d282b36b6f3519aa1f3fc636f609c47dddb294 # v5.0.0"];

struct ReviewedFile {
    path: &'static str,
    sha256: &'static str,
}

impl ReviewedFile {
    const fn new(path: &'static str, sha256: &'static str) -> Self {
        Self { path, sha256 }
    }
}

pub(super) fn validate_configuration(workspace: &Path, tracked_paths: &BTreeSet<String>) -> Result<()> {
    let base_profile_present = classification_profile_present_in_base(workspace)?;
    validate_configuration_against(workspace, tracked_paths, base_profile_present)
}

fn validate_configuration_against(workspace: &Path, tracked_paths: &BTreeSet<String>, base_profile_present: bool) -> Result<()> {
    let mise_toml = reviewed_file(workspace, tracked_paths, "mise.toml")?;
    let mise_lock = reviewed_file(workspace, tracked_paths, "mise.lock")?;
    let profile = (digest(&mise_toml), digest(&mise_lock));
    if !REVIEWED_MISE_PROFILES.iter().any(|reviewed| reviewed.0 == profile.0 && reviewed.1 == profile.1) {
        bail!("mise.toml and mise.lock must match one complete reviewed tool profile before CI may activate Mise");
    }

    let dependency_review = reviewed_file(workspace, tracked_paths, DEPENDENCY_REVIEW_PATH)?;
    let dependency_review = std::str::from_utf8(&dependency_review).context("dependency-review workflow must be UTF-8")?;
    if !REVIEWED_DEPENDENCY_REVIEW_WORKFLOWS.contains(&dependency_review.trim_end()) {
        bail!("{DEPENDENCY_REVIEW_PATH:?} must retain its exact reviewed trigger, permissions, job controls, checkout, and dependency-review step");
    }
    validate_staged_classification_profiles_against(workspace, tracked_paths, PR_CLASSIFICATION_PROFILES, base_profile_present)?;
    Ok(())
}

fn classification_profile_present_in_base(workspace: &Path) -> Result<bool> {
    let Some(revision) = crate::structure::revision::maintainability_base_revision()? else {
        return Ok(false);
    };
    classification_profile_present_at_revision(workspace, &revision)
}

fn classification_profile_present_at_revision(workspace: &Path, revision: &str) -> Result<bool> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args([
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            revision,
            "--",
            PR_CLASSIFICATION_PATH,
            "policy/maintainability/feature-freeze.json",
            "script/check_pr_classification.py",
            PR_CLASSIFICATION_PACKAGE_PREFIX,
            PR_CLASSIFICATION_MODULE_ALIAS,
        ])
        .output()
        .context("inspect the PR-classification profile in a Git revision")?;
    if !output.status.success() {
        bail!("cannot inspect the PR-classification profile in Git revision {revision:?}");
    }
    Ok(!output.stdout.is_empty())
}

fn validate_staged_classification_profiles_against(workspace: &Path, tracked_paths: &BTreeSet<String>, profiles: &[&[ReviewedFile]], base_profile_present: bool) -> Result<()> {
    let Some(first_profile) = profiles.first() else {
        bail!("PR-classification reviewed profiles must not be empty");
    };
    let reviewed_paths = first_profile.iter().map(|reviewed| reviewed.path).collect::<BTreeSet<_>>();
    if reviewed_paths.len() != first_profile.len()
        || profiles
            .iter()
            .any(|profile| profile.iter().map(|reviewed| reviewed.path).collect::<BTreeSet<_>>() != reviewed_paths)
    {
        bail!("PR-classification reviewed profiles must share one unique path inventory");
    }
    let profile_present = tracked_paths
        .iter()
        .any(|path| reviewed_paths.contains(path.as_str()) || path.starts_with(PR_CLASSIFICATION_PACKAGE_PREFIX) || path == PR_CLASSIFICATION_MODULE_ALIAS);
    if !profile_present {
        if base_profile_present {
            bail!("PR-classification runtime cannot be removed after it is present in the protected base revision");
        }
        return Ok(());
    }
    if tracked_paths
        .iter()
        .any(|path| path.starts_with(PR_CLASSIFICATION_PACKAGE_PREFIX) && !reviewed_paths.contains(path.as_str()) || path == PR_CLASSIFICATION_MODULE_ALIAS)
    {
        bail!("PR-classification package inventory contains an unreviewed module");
    }
    for profile in profiles {
        let mut matches = true;
        for reviewed in *profile {
            let contents = reviewed_file(workspace, tracked_paths, reviewed.path)?;
            matches &= digest(&contents) == reviewed.sha256;
        }
        if matches {
            return Ok(());
        }
    }
    bail!("PR-classification runtime inputs do not match any reviewed atomic profile")
}

pub(super) fn validate_mise_action(path: &str, lines: &[&str], uses_index: usize) -> Result<()> {
    let inputs = action_inputs(path, lines, uses_index)?;
    if inputs.len() != 1 || inputs[0].key != "version" || inputs[0].literal(path)? != MISE_VERSION {
        bail!("Mise activation in {path:?} must use only the exact reviewed version {MISE_VERSION:?}");
    }
    Ok(())
}

pub(super) fn validate_dependency_review_reference(path: &str) -> Result<()> {
    if path != DEPENDENCY_REVIEW_PATH {
        bail!("the dependency-review action may run only in the exact guarded workflow {DEPENDENCY_REVIEW_PATH:?}");
    }
    Ok(())
}

fn reviewed_file(workspace: &Path, tracked_paths: &BTreeSet<String>, path: &str) -> Result<Vec<u8>> {
    if !tracked_paths.contains(path) {
        bail!("guarded CI input {path:?} must be tracked");
    }
    let absolute = workspace.join(path);
    let metadata = fs::symlink_metadata(&absolute).with_context(|| format!("inspect guarded CI input {path:?}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("guarded CI input {path:?} must be a regular non-symlink file");
    }
    fs::read(absolute).with_context(|| format!("read guarded CI input {path:?}"))
}

fn digest(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn tracked_paths() -> BTreeSet<String> {
        ["mise.toml", "mise.lock", DEPENDENCY_REVIEW_PATH].into_iter().map(str::to_owned).collect()
    }

    fn fixture() -> tempfile::TempDir {
        let fixture = tempfile::tempdir().expect("temp fixture");
        fs::create_dir_all(fixture.path().join(".github/workflows")).expect("workflow directory");
        for path in ["mise.toml", "mise.lock", DEPENDENCY_REVIEW_PATH] {
            stage_fixture_input(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path), &fixture.path().join(path));
        }
        fixture
    }

    fn stage_fixture_input(source: &Path, destination: &Path) {
        if destination.exists() {
            make_fixture_writable(destination);
        }
        let contents = fs::read(source).expect("read guarded input");
        fs::write(destination, contents).expect("write guarded input fixture");
        make_fixture_writable(destination);
    }

    fn make_fixture_writable(path: &Path) {
        let mut permissions = path.metadata().expect("fixture metadata").permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            permissions.set_mode(permissions.mode() | 0o600);
        }
        #[cfg(windows)]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).expect("make fixture owner-writable");
    }

    fn git(workspace: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git").current_dir(workspace).args(arguments).output().expect("run fixture Git command");
        assert!(output.status.success(), "git {arguments:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).expect("fixture Git output is UTF-8").trim().to_owned()
    }

    fn commit_fixture(workspace: &Path, message: &str) -> String {
        git(workspace, &["add", "--all"]);
        git(
            workspace,
            &[
                "-c",
                "user.name=LocalHold Tests",
                "-c",
                "user.email=tests@localhold.invalid",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
        );
        git(workspace, &["rev-parse", "HEAD"])
    }

    #[test]
    fn guarded_configuration_accepts_the_reviewed_files() {
        let fixture = fixture();
        validate_configuration_against(fixture.path(), &tracked_paths(), false).expect("reviewed configuration");
    }

    #[test]
    fn guarded_fixture_staging_does_not_preserve_read_only_permissions() {
        let source = tempfile::NamedTempFile::new().expect("source fixture");
        fs::write(source.path(), b"reviewed\n").expect("source contents");
        let original_permissions = source.path().metadata().expect("source metadata").permissions();
        let mut read_only_permissions = original_permissions.clone();
        read_only_permissions.set_readonly(true);
        fs::set_permissions(source.path(), read_only_permissions).expect("make source read-only");

        let destination = tempfile::NamedTempFile::new().expect("destination fixture");
        let mut destination_permissions = destination.path().metadata().expect("destination metadata").permissions();
        destination_permissions.set_readonly(true);
        fs::set_permissions(destination.path(), destination_permissions).expect("make destination read-only");
        stage_fixture_input(source.path(), destination.path());
        assert_eq!(fs::read(destination.path()).expect("staged contents"), b"reviewed\n");
        assert!(!destination.path().metadata().expect("destination metadata").permissions().readonly());
        fs::write(destination.path(), b"changed\n").expect("overwrite staged fixture");

        fs::set_permissions(source.path(), original_permissions).expect("restore source permissions");
    }

    #[test]
    fn mise_tool_inventory_and_lockfile_are_one_reviewed_profile() {
        for path in ["mise.toml", "mise.lock"] {
            let fixture = fixture();
            fs::write(fixture.path().join(path), b"unreviewed\n").expect("alter guarded input");
            assert!(validate_configuration_against(fixture.path(), &tracked_paths(), false).is_err(), "accepted altered {path}");
        }
    }

    #[test]
    fn dependency_review_control_flow_is_exact() {
        for alteration in [
            "      - if: ${{ false }}\n        uses: actions/dependency-review-action",
            "      - continue-on-error: true\n        uses: actions/dependency-review-action",
            "      - run: cp quality/Justfile Justfile\n\n      - uses: actions/dependency-review-action",
        ] {
            let fixture = fixture();
            let path = fixture.path().join(DEPENDENCY_REVIEW_PATH);
            let source = fs::read_to_string(&path).expect("read workflow");
            fs::write(&path, source.replace("      - uses: actions/dependency-review-action", alteration)).expect("alter workflow");
            assert!(validate_configuration_against(fixture.path(), &tracked_paths(), false).is_err(), "accepted {alteration:?}");
        }
    }

    #[test]
    fn staged_classification_runtime_rejects_partial_or_extra_profiles() {
        let partial_fixture = fixture();
        fs::write(partial_fixture.path().join(PR_CLASSIFICATION_PATH), b"unreviewed\n").expect("classification workflow fixture");
        let mut paths = tracked_paths();
        paths.insert(PR_CLASSIFICATION_PATH.to_owned());
        assert!(validate_configuration_against(partial_fixture.path(), &paths, false).is_err());

        let extra_fixture = fixture();
        let unexpected = format!("{PR_CLASSIFICATION_PACKAGE_PREFIX}shadow.py");
        let mut paths = tracked_paths();
        paths.insert(unexpected);
        assert!(validate_configuration_against(extra_fixture.path(), &paths, false).is_err());
    }

    #[test]
    fn staged_classification_runtime_can_be_absent_only_before_rollout() {
        let workspace = tempfile::tempdir().expect("classification absence fixture");
        let paths = BTreeSet::new();
        validate_staged_classification_profiles_against(workspace.path(), &paths, PR_CLASSIFICATION_PROFILES, false).expect("pre-rollout absence");
        assert!(
            validate_staged_classification_profiles_against(workspace.path(), &paths, PR_CLASSIFICATION_PROFILES, true).is_err(),
            "accepted complete removal after the base profile was deployed"
        );
    }

    #[test]
    fn base_revision_inventory_detects_package_alias_and_lookup_failures() {
        let workspace = tempfile::tempdir().expect("classification base fixture");
        git(workspace.path(), &["init", "--quiet"]);
        fs::write(workspace.path().join("seed"), b"seed\n").expect("seed fixture repository");
        let absent = commit_fixture(workspace.path(), "absent");
        assert!(!classification_profile_present_at_revision(workspace.path(), &absent).expect("inspect absent profile"));

        let package = workspace.path().join(PR_CLASSIFICATION_PACKAGE_PREFIX).join("runtime.py");
        fs::create_dir_all(package.parent().expect("package parent")).expect("create classifier package");
        fs::write(&package, b"# classifier\n").expect("write classifier package module");
        let packaged = commit_fixture(workspace.path(), "package");
        assert!(classification_profile_present_at_revision(workspace.path(), &packaged).expect("inspect package profile"));

        fs::remove_file(package).expect("remove package module");
        let alias = workspace.path().join(PR_CLASSIFICATION_MODULE_ALIAS);
        fs::write(alias, b"# legacy alias\n").expect("write classifier alias");
        let aliased = commit_fixture(workspace.path(), "alias");
        assert!(classification_profile_present_at_revision(workspace.path(), &aliased).expect("inspect alias profile"));
        assert!(classification_profile_present_at_revision(workspace.path(), "1111111111111111111111111111111111111111").is_err());
    }

    #[test]
    fn staged_classification_runtime_accepts_only_one_complete_profile() {
        const TEST_DIGEST: &str = "a9f2d25d1f71f8065e2119e538bde8846570fcdad320388236e99d9e225c290d";
        let workspace = tempfile::tempdir().expect("classification profile fixture");
        let profile = ORIGINAL_PR_CLASSIFICATION_PROFILE
            .iter()
            .map(|reviewed| ReviewedFile::new(reviewed.path, TEST_DIGEST))
            .collect::<Vec<_>>();
        let mut paths = BTreeSet::new();
        for reviewed in &profile {
            let path = workspace.path().join(reviewed.path);
            fs::create_dir_all(path.parent().expect("profile parent")).expect("profile directory");
            fs::write(&path, b"reviewed\n").expect("profile input");
            paths.insert(reviewed.path.to_owned());
        }
        validate_staged_classification_profiles_against(workspace.path(), &paths, &[&profile], false).expect("complete classification profile");

        fs::write(workspace.path().join(profile[0].path), b"changed\n").expect("alter profile input");
        assert!(validate_staged_classification_profiles_against(workspace.path(), &paths, &[&profile], false).is_err());
    }

    #[test]
    fn staged_classification_profiles_reject_hybrids() {
        const ORIGINAL_DIGEST: &str = "a9f2d25d1f71f8065e2119e538bde8846570fcdad320388236e99d9e225c290d";
        const NEXT_DIGEST: &str = "8a0956311647187d73d47ac672d55da73c8feae40cd9fd177414b72e75e0693f";
        let workspace = tempfile::tempdir().expect("classification profile fixture");
        let original = ORIGINAL_PR_CLASSIFICATION_PROFILE
            .iter()
            .map(|reviewed| ReviewedFile::new(reviewed.path, ORIGINAL_DIGEST))
            .collect::<Vec<_>>();
        let next = ORIGINAL_PR_CLASSIFICATION_PROFILE
            .iter()
            .enumerate()
            .map(|(index, reviewed)| ReviewedFile::new(reviewed.path, if index < 2 { NEXT_DIGEST } else { ORIGINAL_DIGEST }))
            .collect::<Vec<_>>();
        let profiles = [&original[..], &next[..]];
        let mut paths = BTreeSet::new();
        for reviewed in &original {
            let path = workspace.path().join(reviewed.path);
            fs::create_dir_all(path.parent().expect("profile parent")).expect("profile directory");
            fs::write(&path, b"reviewed\n").expect("profile input");
            paths.insert(reviewed.path.to_owned());
        }
        validate_staged_classification_profiles_against(workspace.path(), &paths, &profiles, false).expect("original complete profile");

        fs::write(workspace.path().join(original[0].path), b"next\n").expect("first next-profile input");
        assert!(
            validate_staged_classification_profiles_against(workspace.path(), &paths, &profiles, false).is_err(),
            "accepted hybrid profile"
        );

        fs::write(workspace.path().join(original[1].path), b"next\n").expect("second next-profile input");
        validate_staged_classification_profiles_against(workspace.path(), &paths, &profiles, false).expect("next complete profile");
    }

    #[test]
    fn mise_action_requires_only_the_reviewed_version() {
        for (inputs, accepted) in [
            ("version: 2026.7.5", true),
            ("version: latest", false),
            ("version: 2026.7.5\n      install: false", false),
            ("version: 2026.7.5\n      version: 2026.7.5", false),
        ] {
            let source = format!("steps:\n  - uses: {MISE_ACTION}\n    with:\n      {inputs}\n");
            let lines = source.lines().collect::<Vec<_>>();
            assert_eq!(validate_mise_action(".github/workflows/test.yml", &lines, 1).is_ok(), accepted, "{inputs:?}");
        }
    }
}
