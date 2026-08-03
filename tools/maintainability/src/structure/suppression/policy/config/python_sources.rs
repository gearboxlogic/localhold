use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::parse_nul_paths;

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReviewedSource {
    path: &'static str,
    mode: u32,
    sha256: &'static str,
}

impl ReviewedSource {
    const fn regular(path: &'static str, sha256: &'static str) -> Self {
        Self { path, mode: 0o100_644, sha256 }
    }

    const fn executable(path: &'static str, sha256: &'static str) -> Self {
        Self { path, mode: 0o100_755, sha256 }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ObservedSource {
    path: String,
    mode: u32,
    sha256: String,
}

struct TrackedPath {
    path: String,
    mode: u32,
}

struct ReviewedProfile {
    name: &'static str,
    sources: &'static [ReviewedSource],
}

const CURRENT_PROFILE: &[ReviewedSource] = &[
    ReviewedSource::regular("assets/brand/explorations/fortgen.py", "87c07e9806016fd4bc348ddc6c2e7f9ade770919c4e050c7084d9e9dc64bfca7"),
    ReviewedSource::regular("assets/brand/explorations/round2.py", "6044c0d67fcbaa66c71a547e63487ebf59c22e02fb10ca901e545141bb950459"),
    ReviewedSource::regular("assets/brand/explorations/round3.py", "bcb3af485db2d08a6e3a0c561444c38b565bbe76a8a4b6a2dfc3f5dd71c4fab6"),
    ReviewedSource::regular("script/check-time-abstraction.py", "b35d1da9cf7635c3ba2d12d33036674613da0cc35442cade3b5d3b1a2923c0d3"),
    ReviewedSource::executable("script/database_fixtures.py", "698b288b56e2a16ea4878ec2f009b449fd4cd376d2e3c4358445ae4b4ed1fb3f"),
    ReviewedSource::executable("script/package-release.py", "be1c491277336054ad29c1e3bb6f8a8dd7fb4011c0720b48750aaa7b03161ac7"),
    ReviewedSource::regular("script/package_release.py", "163b91d31ae73bdee732512ac56a615330507a978cfa78d5cd680e008d3a87a4"),
    ReviewedSource::regular("script/prepare-cuda-runtime.py", "77f67ac502da3438ccba06c94abe035111ecfa56616bdb8c1eeba55b0f5074be"),
    ReviewedSource::regular("script/prepare_cuda_runtime.py", "dbad298e363fefdc0a557fa023c943337aa0d423794d78daf8fa7de9fe5dd494"),
    ReviewedSource::executable("script/release.py", "81490a55ea69c1119411621a9d1da558bae8b574d16ac9e57e069e73f3c284ea"),
    ReviewedSource::regular("script/run-python-tests.py", "a916b28f12fc6e6b3370bfe2f24a331f39488a2f2ce184b23a6c645cc2dec85f"),
    ReviewedSource::regular("script/tests/test_cuda_release.py", "850414f812aeddcf692d47b9cff1a820959aafea2fc25443161739010b6b850f"),
    ReviewedSource::regular("script/tests/test_database_fixtures.py", "616df3e8d2f444fcd24a0b668eb3e492100fc465f53f041e7a0ca41555247b57"),
    ReviewedSource::regular("script/tests/test_release.py", "5c256b12ce05d25751b10adbcc7a3ba78bacd1b0b9f6f29b360c7c1b648438d0"),
    ReviewedSource::regular("script/tests/test_time_abstraction.py", "b797b46d0f6c1ebe3ef8496dfa7e1e6e81d02190430dd62b8b3ae83282e07c40"),
    ReviewedSource::regular("script/validate-cuda-runtime.py", "61d9779e09753d39e5f40fd5bdc23fa0b8deef231363a583d262857649057c15"),
    ReviewedSource::regular("script/validate_cuda_runtime.py", "53b684a7e00c9bad1358ccd5baafa5b2039f4be706f2205a8b9bafc461623151"),
];

const FEATURE_FREEZE_DELIVERY_PROFILE: &[ReviewedSource] = &[
    ReviewedSource::regular("assets/brand/explorations/fortgen.py", "87c07e9806016fd4bc348ddc6c2e7f9ade770919c4e050c7084d9e9dc64bfca7"),
    ReviewedSource::regular("assets/brand/explorations/round2.py", "6044c0d67fcbaa66c71a547e63487ebf59c22e02fb10ca901e545141bb950459"),
    ReviewedSource::regular("assets/brand/explorations/round3.py", "bcb3af485db2d08a6e3a0c561444c38b565bbe76a8a4b6a2dfc3f5dd71c4fab6"),
    ReviewedSource::regular("script/check-time-abstraction.py", "b35d1da9cf7635c3ba2d12d33036674613da0cc35442cade3b5d3b1a2923c0d3"),
    ReviewedSource::regular("script/check_pr_classification.py", "64f498229401c518ee377b5a74ec9f9c4c946b424316b49e979d5155469720e2"),
    ReviewedSource::executable("script/database_fixtures.py", "c3398a1b9945165e875f457aec3d6f5e60d5c06683eed2e0a82b64644219a61d"),
    ReviewedSource::executable("script/package-release.py", "be1c491277336054ad29c1e3bb6f8a8dd7fb4011c0720b48750aaa7b03161ac7"),
    ReviewedSource::regular("script/package_release.py", "f3ec254d0fdf9d58b3f7f9f950e950b86af5574fefa13e02e71165af75d08c99"),
    ReviewedSource::regular("script/pr_classification/__init__.py", "a8ee1ff16a8e133d6c930231522ca7803b69d3e81b2f9b7ad43b8841a89b3705"),
    ReviewedSource::regular("script/pr_classification/github_api.py", "f6bb2b19274b6c207dafba9dd18cb8eb1611dcde9f2f9ac328f3a0de3c4c76c5"),
    ReviewedSource::regular("script/pr_classification/markdown.py", "ecfc33f63804491d99bfce35dc440bade7bf84bb9cca68753e1dfa865a99b822"),
    ReviewedSource::regular("script/pr_classification/model.py", "822d5b19e91a6691ebb26249d65d3e6381a6e016766a3c6edfe07cdff83b2d82"),
    ReviewedSource::regular("script/pr_classification/policy.py", "6fe2900394d66e25c78bfa16de90c93275abeab5f179ba9cbf33570e89dec230"),
    ReviewedSource::regular("script/pr_classification/reviews.py", "d26ec855a798f5b3df7ab205c620cf8bb4bb69429c150d437cf89567a4bcab19"),
    ReviewedSource::regular("script/pr_classification/validation.py", "ff679b94f3eec0c9464166e4d160aa4f4a2a9950695d55acb2dcb5e226c9c6fc"),
    ReviewedSource::regular("script/prepare-cuda-runtime.py", "77f67ac502da3438ccba06c94abe035111ecfa56616bdb8c1eeba55b0f5074be"),
    ReviewedSource::regular("script/prepare_cuda_runtime.py", "b910ba9e57138f9381b02b154cae84c7c8f1ad1c4e2de510dd90fd9f3f727756"),
    ReviewedSource::executable("script/release.py", "be6e7ba8613f8ea646043eab3c60ea352373e6d8c962361dc6e04e940ccc8e79"),
    ReviewedSource::regular("script/run-python-tests.py", "a916b28f12fc6e6b3370bfe2f24a331f39488a2f2ce184b23a6c645cc2dec85f"),
    ReviewedSource::regular("script/tests/test_cuda_release.py", "9b78de542a72594628965dff2d15d100463f128c93cb98dcab39ebf289a7ced3"),
    ReviewedSource::regular("script/tests/test_database_fixtures.py", "12ee731aaeebc0c033354315ac2e4f464d97f9e1342bdad2f844e90a408d3b49"),
    ReviewedSource::regular("script/tests/test_pr_classification.py", "f1b3899c9fc7a949213587827cefceb254f1a36d0256d6547a4dd14177bcb2af"),
    ReviewedSource::regular(
        "script/tests/test_pr_classification_reviews.py",
        "8725b8c769a5a5a9a63a1def2f0f5a99905b83e6dd9624fddcc7b39345303b41",
    ),
    ReviewedSource::regular(
        "script/tests/test_pr_classification_workflow.py",
        "2a26de1bac28470c0280b10c98000e8bb5f0a5ef1f81e135e0b3b10ef890a061",
    ),
    ReviewedSource::regular("script/tests/test_release.py", "003d76b2c637776160ea55f7b9d89f31234e9fea4ba349092062ca04274901fd"),
    ReviewedSource::regular("script/tests/test_time_abstraction.py", "b797b46d0f6c1ebe3ef8496dfa7e1e6e81d02190430dd62b8b3ae83282e07c40"),
    ReviewedSource::regular("script/validate-cuda-runtime.py", "61d9779e09753d39e5f40fd5bdc23fa0b8deef231363a583d262857649057c15"),
    ReviewedSource::regular("script/validate_cuda_runtime.py", "53b684a7e00c9bad1358ccd5baafa5b2039f4be706f2205a8b9bafc461623151"),
];

const REVIEWED_PROFILES: &[ReviewedProfile] = &[
    ReviewedProfile {
        name: "current",
        sources: CURRENT_PROFILE,
    },
    ReviewedProfile {
        name: "feature-freeze delivery",
        sources: FEATURE_FREEZE_DELIVERY_PROFILE,
    },
];

pub(in crate::structure::suppression::policy) fn validate(workspace: &Path) -> Result<()> {
    let tracked = tracked_paths(workspace)?;
    let untracked = untracked_paths(workspace)?;
    reject_unsupported_python_entrypoints(workspace, tracked.iter().map(|entry| entry.path.as_str()).chain(untracked.iter().map(String::as_str)))?;
    if let Some(path) = untracked.iter().find(|path| has_extension(path, "py")) {
        bail!("untracked Python source is outside every reviewed profile: {path:?}");
    }
    let observed = observed_sources(workspace, &tracked)?;
    let matching = REVIEWED_PROFILES.iter().filter(|profile| profile_matches(profile, &observed)).collect::<Vec<_>>();
    if matching.len() == 1 {
        return Ok(());
    }
    if matching.len() > 1 {
        let names = matching.iter().map(|profile| profile.name).collect::<Vec<_>>().join(", ");
        bail!("Python source tree ambiguously matches multiple reviewed profiles: {names}");
    }
    let names = REVIEWED_PROFILES.iter().map(|profile| profile.name).collect::<Vec<_>>().join(", ");
    bail!("Python source tree does not match an atomic reviewed profile ({names}); preauthorize the complete source profile before changing Python files")
}

fn tracked_paths(workspace: &Path) -> Result<Vec<TrackedPath>> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-files", "-z", "--stage", "--cached"])
        .output()
        .context("list tracked Python source profile inputs")?;
    if !output.status.success() {
        bail!("git ls-files failed while listing tracked Python source profile inputs");
    }
    let mut paths = Vec::new();
    for record in output.stdout.split(|byte| *byte == b'\0').filter(|record| !record.is_empty()) {
        let record = std::str::from_utf8(record).context("tracked Python profile entry is not UTF-8")?;
        let (metadata, path) = record.split_once('\t').context("tracked Python profile entry has no path")?;
        super::validate_relative_path(path, "tracked Python profile path")?;
        let mut fields = metadata.split_whitespace();
        let mode = fields.next().context("tracked Python profile entry has no mode")?;
        let _object = fields.next().context("tracked Python profile entry has no object ID")?;
        if fields.next() != Some("0") || fields.next().is_some() {
            bail!("tracked Python profile has an unresolved index entry: {path:?}");
        }
        paths.push(TrackedPath {
            path: path.to_owned(),
            mode: u32::from_str_radix(mode, 8).context("tracked Python profile mode is not octal")?,
        });
    }
    paths.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(paths)
}

fn untracked_paths(workspace: &Path) -> Result<Vec<String>> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-files", "-z", "--others", "--exclude-standard"])
        .output()
        .context("list untracked Python source profile inputs")?;
    if !output.status.success() {
        bail!("git ls-files failed while listing untracked Python source profile inputs");
    }
    parse_nul_paths(&output.stdout, |_| true)
}

fn observed_sources(workspace: &Path, paths: &[TrackedPath]) -> Result<Vec<ObservedSource>> {
    paths
        .iter()
        .filter(|entry| has_extension(&entry.path, "py"))
        .map(|entry| {
            let absolute = workspace.join(&entry.path);
            let metadata = fs::symlink_metadata(&absolute).with_context(|| format!("inspect Python source profile input {}", entry.path))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("Python source profile input must be a regular non-symlink file: {:?}", entry.path);
            }
            let source = fs::read(&absolute).with_context(|| format!("read Python source profile input {}", entry.path))?;
            Ok(ObservedSource {
                path: entry.path.clone(),
                mode: entry.mode,
                sha256: format!("{:x}", Sha256::digest(source)),
            })
        })
        .collect()
}

fn profile_matches(profile: &ReviewedProfile, observed: &[ObservedSource]) -> bool {
    profile.sources.len() == observed.len()
        && profile
            .sources
            .iter()
            .zip(observed)
            .all(|(expected, actual)| expected.path == actual.path && expected.mode == actual.mode && expected.sha256 == actual.sha256)
}

fn reject_unsupported_python_entrypoints<'a>(workspace: &Path, paths: impl Iterator<Item = &'a str>) -> Result<()> {
    for path in paths {
        if has_extension(path, "pyw") {
            bail!("Python .pyw execution sources are unsupported; use a reviewed .py source: {path:?}");
        }
        if !has_extension(path, "py") && has_unsupported_shebang(&workspace.join(path))? {
            bail!("extensionless Python or dynamically selected interpreter sources are unsupported; use a reviewed .py source: {path:?}");
        }
    }
    Ok(())
}

fn has_extension(path: &str, expected: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

fn has_unsupported_shebang(path: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("inspect possible Python entrypoint {}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(true);
    }
    if metadata.len() == 0 {
        return Ok(false);
    }
    let mut prefix = [0_u8; 256];
    let count = File::open(path)
        .with_context(|| format!("open possible Python entrypoint {}", path.display()))?
        .read(&mut prefix)
        .with_context(|| format!("read possible Python entrypoint {}", path.display()))?;
    let first_line = prefix[..count].split(|byte| *byte == b'\n').next().unwrap_or_default();
    let Some(interpreter) = first_line.strip_prefix(b"#!") else {
        return Ok(false);
    };
    let Ok(interpreter) = std::str::from_utf8(interpreter) else {
        return Ok(true);
    };
    Ok(shebang_requires_python_review(interpreter.trim_end_matches('\r')))
}

fn shebang_requires_python_review(interpreter: &str) -> bool {
    let words = interpreter.split_whitespace().collect::<Vec<_>>();
    let Some(command) = words.first() else {
        return false;
    };
    if command.starts_with('[') {
        return false;
    }
    if is_direct_shell_interpreter(command) {
        return words.len() != 1;
    }
    if is_direct_env_interpreter(command) {
        return !matches!(words.as_slice(), [_, shell] if is_env_shell_interpreter(shell));
    }
    true
}

fn is_direct_shell_interpreter(command: &str) -> bool {
    matches!(command, "/bin/bash" | "/usr/bin/bash" | "/bin/sh" | "/usr/bin/sh")
}

fn is_direct_env_interpreter(command: &str) -> bool {
    matches!(command, "/bin/env" | "/usr/bin/env")
}

fn is_env_shell_interpreter(command: &str) -> bool {
    matches!(command, "bash" | "sh")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_python_tree_matches_one_atomic_profile() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        validate(&workspace).expect("reviewed Python source profile");
    }

    #[test]
    fn profile_matching_rejects_mutation_deletion_addition_and_hybrids() {
        let mut changed = observed(CURRENT_PROFILE);
        changed[0].sha256 = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_owned();
        assert!(!REVIEWED_PROFILES.iter().any(|profile| profile_matches(profile, &changed)));

        let mut deleted = observed(CURRENT_PROFILE);
        deleted.pop();
        assert!(!REVIEWED_PROFILES.iter().any(|profile| profile_matches(profile, &deleted)));

        let mut added = observed(CURRENT_PROFILE);
        added.push(ObservedSource {
            path: "script/copied.py".to_owned(),
            mode: CURRENT_PROFILE[0].mode,
            sha256: CURRENT_PROFILE[0].sha256.to_owned(),
        });
        assert!(!REVIEWED_PROFILES.iter().any(|profile| profile_matches(profile, &added)));

        let mut renamed = observed(CURRENT_PROFILE);
        renamed[0].path = "assets/brand/explorations/copied.py".to_owned();
        assert!(!REVIEWED_PROFILES.iter().any(|profile| profile_matches(profile, &renamed)));

        let mut hybrid = observed(CURRENT_PROFILE);
        let current = hybrid.iter_mut().find(|source| source.path == "script/database_fixtures.py").expect("current source");
        current.sha256 = FEATURE_FREEZE_DELIVERY_PROFILE
            .iter()
            .find(|source| source.path == "script/database_fixtures.py")
            .expect("pending source")
            .sha256
            .to_owned();
        assert!(!REVIEWED_PROFILES.iter().any(|profile| profile_matches(profile, &hybrid)));

        let mut wrong_mode = observed(CURRENT_PROFILE);
        wrong_mode[0].mode = 0o100_755;
        assert!(!REVIEWED_PROFILES.iter().any(|profile| profile_matches(profile, &wrong_mode)));
    }

    #[test]
    fn exact_current_and_pending_profiles_are_accepted_without_cross_matching() {
        let current = observed(CURRENT_PROFILE);
        let pending = observed(FEATURE_FREEZE_DELIVERY_PROFILE);
        assert!(profile_matches(&REVIEWED_PROFILES[0], &current));
        assert!(!profile_matches(&REVIEWED_PROFILES[0], &pending));
        assert!(!profile_matches(&REVIEWED_PROFILES[1], &current));
        assert!(profile_matches(&REVIEWED_PROFILES[1], &pending));
    }

    #[test]
    fn unsupported_python_entrypoints_are_identified() {
        assert!(has_extension("script/CHECK.PY", "py"));
        assert!(has_extension("script/check.PYW", "pyw"));
        assert!(!has_extension("script/check.txt", "py"));

        let workspace = tempfile::tempdir().expect("temporary workspace");
        for (name, shebang) in [
            ("python", "#!/usr/bin/python3\n"),
            ("free-threaded-python", "#!/usr/bin/python3.13t\n"),
            ("env-python", "#!/usr/bin/env python3\n"),
            ("env-s-python", "#!/usr/bin/env -S python3 -I\n"),
            ("env-s-attached-python", "#!/usr/bin/env -Spython3 -I\n"),
            ("env-s-long-python", "#!/usr/bin/env --split-string=python3 -I\n"),
            ("env-s-long-quoted-python", "#!/usr/bin/env --split-string='python3 -I'\n"),
            ("env-s-assignment-python", "#!/usr/bin/env -S FOO=bar python3 -I\n"),
            ("env-s-dashed-assignment-python", "#!/usr/bin/env -S A-B=x python3 -I\n"),
            ("env-s-numeric-assignment-python", "#!/usr/bin/env -S 1A=x python3 -I\n"),
            ("env-s-empty-name-assignment-python", "#!/usr/bin/env -S =x python3 -I\n"),
            ("env-s-option-python", "#!/usr/bin/env -S -i python3 -I\n"),
            ("env-s-unset-python", "#!/usr/bin/env -S --unset=PATH /usr/bin/python3 -I\n"),
            ("env-s-unset-operand-python", "#!/usr/bin/env -S -u PYTHONPATH python3\n"),
            ("env-s-chdir-python", "#!/usr/bin/env --split-string=-C /tmp python3\n"),
            ("env-s-concatenated-python", "#!/usr/bin/env -S py\"\"thon3 -I\n"),
            ("env-s-dynamic-python", "#!/usr/bin/env -S ${PYTHON} -I\n"),
            ("env-unset-python", "#!/usr/bin/env -u PYTHONPATH python3\n"),
            ("env-chdir-python", "#!/usr/bin/env --chdir /tmp python3\n"),
            ("env-argv0-python", "#!/usr/bin/env -a custom python3\n"),
            ("env-helper", "#!/usr/bin/env python-helper\n"),
            ("env-s-wrapper-python", "#!/usr/bin/env -S env python3 -I\n"),
            ("fake-env-shell", "#!/tmp/env bash\n"),
            ("nice-python", "#!/usr/bin/nice python3\n"),
            ("pypy", "#!/opt/bin/pypy3\n"),
        ] {
            let path = workspace.path().join(name);
            fs::write(&path, shebang).expect("Python shebang fixture");
            assert!(has_unsupported_shebang(&path).expect("inspect Python shebang"), "{name}");
        }
        for (name, shebang) in [("shell", "#!/bin/sh\n"), ("env-shell", "#!/usr/bin/env bash\n")] {
            let path = workspace.path().join(name);
            fs::write(&path, shebang).expect("shell shebang fixture");
            assert!(!has_unsupported_shebang(&path).expect("inspect shell shebang"), "{name}");
        }
        for (name, shebang) in [
            ("shell-argument", "#!/bin/sh python-helper\n"),
            ("env-s-shell", "#!/usr/bin/env -S sh -c python3\n"),
            ("env-shell-mutation", "#!/usr/bin/env -S BASH_ENV=/tmp/helper bash\n"),
        ] {
            let path = workspace.path().join(name);
            fs::write(&path, shebang).expect("unsupported shebang fixture");
            assert!(has_unsupported_shebang(&path).expect("inspect unsupported shebang"), "{name}");
        }

        let rust_attribute = workspace.path().join("rust-inner-attribute");
        fs::write(&rust_attribute, "#![expect(missing_docs)]\n").expect("Rust inner-attribute fixture");
        assert!(!has_unsupported_shebang(&rust_attribute).expect("inspect Rust inner attribute"));

        let encoded = workspace.path().join("latin-1");
        fs::write(&encoded, b"#!/usr/bin/python3\n# coding: latin-1\nvalue = '\xff'\n").expect("non-UTF-8 Python fixture");
        assert!(has_unsupported_shebang(&encoded).expect("inspect non-UTF-8 Python shebang"));

        let non_utf8_interpreter = workspace.path().join("non-utf8-interpreter");
        fs::write(&non_utf8_interpreter, b"#!/tmp/py\xff\n").expect("non-UTF-8 interpreter fixture");
        assert!(has_unsupported_shebang(&non_utf8_interpreter).expect("inspect non-UTF-8 interpreter"));

        #[cfg(unix)]
        {
            let symlink = workspace.path().join("entrypoint-symlink");
            std::os::unix::fs::symlink("shell", &symlink).expect("entrypoint symlink fixture");
            assert!(has_unsupported_shebang(&symlink).expect("inspect entrypoint symlink"));
        }
    }

    #[test]
    fn untracked_python_source_is_rejected_before_profile_matching() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir_all(workspace.path().join("script")).expect("script directory");
        fs::write(workspace.path().join("script/untracked.py"), "print('unreviewed')\n").expect("untracked Python source");
        let status = std::process::Command::new("git")
            .current_dir(workspace.path())
            .args(["init", "--quiet"])
            .status()
            .expect("initialize repository");
        assert!(status.success());

        let error = validate(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("untracked Python source"), "{error:#}");
    }

    fn observed(profile: &[ReviewedSource]) -> Vec<ObservedSource> {
        profile
            .iter()
            .map(|source| ObservedSource {
                path: source.path.to_owned(),
                mode: source.mode,
                sha256: source.sha256.to_owned(),
            })
            .collect()
    }
}
