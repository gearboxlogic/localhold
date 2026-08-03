use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::{digest, reviewed_file};

const WORKFLOW_PATH: &str = ".github/workflows/pr-classification.yml";
const PACKAGE_PREFIX: &str = "script/pr_classification/";
const MODULE_ALIAS: &str = "script/pr_classification.py";
const GITHUB_API_DIGEST: &str = concat!("f6bb2b19274b6c20", "7dafba9dd18cb8eb", "1611dcde9f2f9ac3", "28f3a0de3c4c76c5");
const HARDENED_PROFILE: &[ReviewedFile] = &[
    ReviewedFile::new(WORKFLOW_PATH, "25c11bb0bd514363df2a539d9ceb88d8c954b0c92ec0d9d9e70b9bde32f8b6dd"),
    ReviewedFile::new(
        "policy/maintainability/feature-freeze.json",
        "13086fe757b5613bd3faec4b4f5228df6d3413da8c6474452e6a604621340048",
    ),
    ReviewedFile::new("script/check_pr_classification.py", "64f498229401c518ee377b5a74ec9f9c4c946b424316b49e979d5155469720e2"),
    ReviewedFile::new("script/pr_classification/__init__.py", "a8ee1ff16a8e133d6c930231522ca7803b69d3e81b2f9b7ad43b8841a89b3705"),
    ReviewedFile::new("script/pr_classification/github_api.py", GITHUB_API_DIGEST),
    ReviewedFile::new("script/pr_classification/markdown.py", "ecfc33f63804491d99bfce35dc440bade7bf84bb9cca68753e1dfa865a99b822"),
    ReviewedFile::new("script/pr_classification/model.py", "822d5b19e91a6691ebb26249d65d3e6381a6e016766a3c6edfe07cdff83b2d82"),
    ReviewedFile::new("script/pr_classification/policy.py", "6fe2900394d66e25c78bfa16de90c93275abeab5f179ba9cbf33570e89dec230"),
    ReviewedFile::new("script/pr_classification/reviews.py", "d26ec855a798f5b3df7ab205c620cf8bb4bb69429c150d437cf89567a4bcab19"),
    ReviewedFile::new("script/pr_classification/validation.py", "ff679b94f3eec0c9464166e4d160aa4f4a2a9950695d55acb2dcb5e226c9c6fc"),
];
const PROFILES: &[&[ReviewedFile]] = &[HARDENED_PROFILE];

// Add a successor profile before changing guarded files. Once the successor is
// active in the protected base, older profiles cannot be restored.
struct ReviewedFile {
    path: &'static str,
    sha256: &'static str,
}

impl ReviewedFile {
    const fn new(path: &'static str, sha256: &'static str) -> Self {
        Self { path, sha256 }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BaseProfile {
    Unchecked,
    Absent,
    Reviewed(usize),
    Unrecognized,
}

pub(super) fn profile_in_base(workspace: &Path) -> Result<BaseProfile> {
    let Some(revision) = crate::structure::revision::maintainability_base_revision()? else {
        return Ok(BaseProfile::Unchecked);
    };
    profile_at_revision(workspace, &revision, PROFILES)
}

pub(super) fn validate(workspace: &Path, tracked_paths: &BTreeSet<String>, base_profile: BaseProfile) -> Result<()> {
    validate_against(workspace, tracked_paths, PROFILES, base_profile)
}

fn validate_against(workspace: &Path, tracked_paths: &BTreeSet<String>, profiles: &[&[ReviewedFile]], base_profile: BaseProfile) -> Result<()> {
    let reviewed_paths = reviewed_profile_paths(profiles)?;
    let profile_present = tracked_paths
        .iter()
        .any(|path| reviewed_paths.contains(path.as_str()) || path.starts_with(PACKAGE_PREFIX) || path == MODULE_ALIAS);
    if !profile_present {
        match base_profile {
            BaseProfile::Unchecked | BaseProfile::Absent => return Ok(()),
            BaseProfile::Reviewed(_) | BaseProfile::Unrecognized => {
                bail!("PR-classification runtime cannot be removed after it appears in the protected base revision")
            }
        }
    }
    if tracked_paths
        .iter()
        .any(|path| path.starts_with(PACKAGE_PREFIX) && !reviewed_paths.contains(path.as_str()) || path == MODULE_ALIAS)
    {
        bail!("PR-classification package inventory contains an unreviewed module");
    }
    let candidate = matching_profile(workspace, tracked_paths, profiles)?.context("PR-classification runtime inputs do not match any reviewed atomic profile")?;
    match base_profile {
        BaseProfile::Absent if candidate != 0 => bail!("PR-classification rollout must begin with the first reviewed atomic profile"),
        BaseProfile::Reviewed(base) if candidate < base => bail!("PR-classification runtime cannot downgrade from reviewed profile {base} to {candidate}"),
        BaseProfile::Reviewed(base) if candidate > base + 1 => {
            bail!("PR-classification runtime cannot skip from reviewed profile {base} to {candidate}")
        }
        BaseProfile::Unrecognized => bail!("PR-classification runtime in the protected base revision is not a recognized atomic profile"),
        BaseProfile::Unchecked | BaseProfile::Absent | BaseProfile::Reviewed(_) => Ok(()),
    }
}

fn profile_at_revision(workspace: &Path, revision: &str, profiles: &[&[ReviewedFile]]) -> Result<BaseProfile> {
    let reviewed_paths = reviewed_profile_paths(profiles)?;
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args([
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            revision,
            "--",
            WORKFLOW_PATH,
            "policy/maintainability/feature-freeze.json",
            "script/check_pr_classification.py",
            PACKAGE_PREFIX,
            MODULE_ALIAS,
        ])
        .output()
        .context("inspect the PR-classification profile in a Git revision")?;
    if !output.status.success() {
        bail!("cannot inspect the PR-classification profile in Git revision {revision:?}");
    }
    if output.stdout.is_empty() {
        return Ok(BaseProfile::Absent);
    }
    let revision_paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| std::str::from_utf8(path).context("PR-classification profile path in Git is not UTF-8"))
        .collect::<Result<BTreeSet<_>>>()?;
    if revision_paths != reviewed_paths {
        return Ok(BaseProfile::Unrecognized);
    }

    let mut observed = Vec::with_capacity(reviewed_paths.len());
    for path in reviewed_paths {
        let object = format!("{revision}:{path}");
        let contents = crate::structure::revision::git_command()
            .current_dir(workspace)
            .args(["-c", "core.attributesFile=/dev/null", "show", "--no-ext-diff", "--no-textconv", &object])
            .output()
            .with_context(|| format!("read PR-classification profile input {path:?} from Git revision"))?;
        if !contents.status.success() {
            bail!("cannot read PR-classification profile input {path:?} from Git revision {revision:?}");
        }
        observed.push((path, digest(&contents.stdout)));
    }
    let matching = profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| {
            profile
                .iter()
                .all(|reviewed| observed.iter().any(|(path, sha256)| *path == reviewed.path && sha256 == reviewed.sha256))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    Ok(match matching.as_slice() {
        [index] => BaseProfile::Reviewed(*index),
        _ => BaseProfile::Unrecognized,
    })
}

fn reviewed_profile_paths<'a>(profiles: &'a [&[ReviewedFile]]) -> Result<BTreeSet<&'a str>> {
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
    Ok(reviewed_paths)
}

fn matching_profile(workspace: &Path, tracked_paths: &BTreeSet<String>, profiles: &[&[ReviewedFile]]) -> Result<Option<usize>> {
    let mut matching = None;
    for (index, profile) in profiles.iter().enumerate() {
        let mut matches = true;
        for reviewed in *profile {
            let contents = reviewed_file(workspace, tracked_paths, reviewed.path)?;
            matches &= digest(&contents) == reviewed.sha256;
        }
        if matches && matching.replace(index).is_some() {
            bail!("PR-classification runtime matches more than one reviewed atomic profile");
        }
    }
    Ok(matching)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::*;

    const REVIEWED_DIGEST: &str = "a9f2d25d1f71f8065e2119e538bde8846570fcdad320388236e99d9e225c290d";
    const NEXT_DIGEST: &str = "8a0956311647187d73d47ac672d55da73c8feae40cd9fd177414b72e75e0693f";
    const THIRD_DIGEST: &str = "5eef8098ed6ec0a16249fc7c12422027fc9fd75b16130cc9382cf09102014796";

    fn git(workspace: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git").current_dir(workspace).args(arguments).output().expect("run fixture Git command");
        assert!(output.status.success(), "git {arguments:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).expect("fixture Git output is UTF-8").trim().to_owned()
    }

    fn commit(workspace: &Path, message: &str) -> String {
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

    fn test_profile(digests: impl Fn(usize) -> &'static str) -> Vec<ReviewedFile> {
        HARDENED_PROFILE
            .iter()
            .enumerate()
            .map(|(index, reviewed)| ReviewedFile::new(reviewed.path, digests(index)))
            .collect()
    }

    fn stage_profile(workspace: &Path, profile: &[ReviewedFile], contents: &[u8]) -> BTreeSet<String> {
        let mut paths = BTreeSet::new();
        for reviewed in profile {
            let path = workspace.join(reviewed.path);
            fs::create_dir_all(path.parent().expect("profile parent")).expect("profile directory");
            fs::write(path, contents).expect("profile input");
            paths.insert(reviewed.path.to_owned());
        }
        paths
    }

    #[test]
    fn runtime_can_be_absent_only_before_rollout() {
        let workspace = tempfile::tempdir().expect("classification absence fixture");
        let paths = BTreeSet::new();
        validate(workspace.path(), &paths, BaseProfile::Unchecked).expect("unchecked local absence");
        validate(workspace.path(), &paths, BaseProfile::Absent).expect("pre-rollout absence");
        assert!(validate(workspace.path(), &paths, BaseProfile::Reviewed(0)).is_err(), "accepted removal after rollout");
    }

    #[test]
    fn partial_extra_and_legacy_profiles_are_rejected() {
        let partial = tempfile::tempdir().expect("partial profile fixture");
        fs::create_dir_all(partial.path().join(".github/workflows")).expect("workflow directory");
        fs::write(partial.path().join(WORKFLOW_PATH), b"unreviewed\n").expect("classification workflow fixture");
        assert!(validate(partial.path(), &BTreeSet::from([WORKFLOW_PATH.to_owned()]), BaseProfile::Absent).is_err());

        for unexpected in [format!("{PACKAGE_PREFIX}shadow.py"), MODULE_ALIAS.to_owned()] {
            let workspace = tempfile::tempdir().expect("extra profile fixture");
            assert!(validate(workspace.path(), &BTreeSet::from([unexpected]), BaseProfile::Absent).is_err());
        }
    }

    #[test]
    fn base_revision_inventory_and_contents_are_authenticated() {
        let workspace = tempfile::tempdir().expect("classification base fixture");
        git(workspace.path(), &["init", "--quiet"]);
        fs::write(workspace.path().join("seed"), b"seed\n").expect("seed fixture repository");
        let absent = commit(workspace.path(), "absent");
        assert_eq!(profile_at_revision(workspace.path(), &absent, PROFILES).unwrap(), BaseProfile::Absent);

        let extra = workspace.path().join(PACKAGE_PREFIX).join("runtime.py");
        fs::create_dir_all(extra.parent().expect("package parent")).expect("create classifier package");
        fs::write(&extra, b"# classifier\n").expect("write classifier package module");
        let unrecognized = commit(workspace.path(), "unrecognized");
        assert_eq!(profile_at_revision(workspace.path(), &unrecognized, PROFILES).unwrap(), BaseProfile::Unrecognized);
        assert!(profile_at_revision(workspace.path(), "1111111111111111111111111111111111111111", PROFILES).is_err());

        fs::remove_file(extra).expect("remove unrecognized module");
        let profile = test_profile(|_| REVIEWED_DIGEST);
        stage_profile(workspace.path(), &profile, b"reviewed\n");
        let reviewed = commit(workspace.path(), "reviewed");
        assert_eq!(profile_at_revision(workspace.path(), &reviewed, &[&profile]).unwrap(), BaseProfile::Reviewed(0));

        fs::write(workspace.path().join(profile[0].path), b"changed\n").expect("alter profile input");
        let changed = commit(workspace.path(), "changed");
        assert_eq!(profile_at_revision(workspace.path(), &changed, &[&profile]).unwrap(), BaseProfile::Unrecognized);
    }

    #[test]
    fn runtime_requires_one_complete_atomic_profile() {
        let workspace = tempfile::tempdir().expect("classification profile fixture");
        let profile = test_profile(|_| REVIEWED_DIGEST);
        let paths = stage_profile(workspace.path(), &profile, b"reviewed\n");
        assert_eq!(matching_profile(workspace.path(), &paths, &[&profile]).unwrap(), Some(0));
        assert!(
            matching_profile(workspace.path(), &paths, &[&profile, &profile]).is_err(),
            "accepted an ambiguous reviewed profile"
        );

        fs::write(workspace.path().join(profile[0].path), b"changed\n").expect("alter profile input");
        assert_eq!(matching_profile(workspace.path(), &paths, &[&profile]).unwrap(), None);
    }

    #[test]
    fn profile_transitions_reject_hybrids_skips_and_downgrades() {
        let workspace = tempfile::tempdir().expect("classification profile fixture");
        let original = test_profile(|_| REVIEWED_DIGEST);
        let next = test_profile(|index| if index < 2 { NEXT_DIGEST } else { REVIEWED_DIGEST });
        let third = test_profile(|index| if index < 3 { THIRD_DIGEST } else { REVIEWED_DIGEST });
        let profiles = [&original[..], &next[..], &third[..]];
        let paths = stage_profile(workspace.path(), &original, b"reviewed\n");
        validate_against(workspace.path(), &paths, &profiles, BaseProfile::Unchecked).expect("unchecked local profile");
        validate_against(workspace.path(), &paths, &profiles, BaseProfile::Absent).expect("initial profile");
        assert!(
            validate_against(workspace.path(), &paths, &profiles, BaseProfile::Unrecognized).is_err(),
            "accepted an unknown base profile"
        );

        fs::write(workspace.path().join(original[0].path), b"next\n").expect("first next input");
        assert!(
            validate_against(workspace.path(), &paths, &profiles, BaseProfile::Reviewed(0)).is_err(),
            "accepted hybrid profile"
        );

        fs::write(workspace.path().join(original[1].path), b"next\n").expect("second next input");
        validate_against(workspace.path(), &paths, &profiles, BaseProfile::Reviewed(0)).expect("reviewed upgrade");
        assert!(
            validate_against(workspace.path(), &paths, &profiles, BaseProfile::Absent).is_err(),
            "accepted skipped initial profile"
        );

        for reviewed in original.iter().take(3) {
            fs::write(workspace.path().join(reviewed.path), b"third\n").expect("third profile input");
        }
        assert!(
            validate_against(workspace.path(), &paths, &profiles, BaseProfile::Reviewed(0)).is_err(),
            "accepted a skipped reviewed successor"
        );
        validate_against(workspace.path(), &paths, &profiles, BaseProfile::Reviewed(1)).expect("immediate reviewed successor");

        for reviewed in original.iter().take(3) {
            fs::write(workspace.path().join(reviewed.path), b"reviewed\n").expect("restore original input");
        }
        assert!(
            validate_against(workspace.path(), &paths, &profiles, BaseProfile::Reviewed(1)).is_err(),
            "accepted downgrade"
        );
    }
}
