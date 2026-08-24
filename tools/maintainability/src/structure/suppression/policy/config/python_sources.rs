use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::{ignored_python_paths, parse_nul_paths};

mod profile;
mod revision;

const POLICY_PATH: &str = "policy/maintainability/python-source-profile.json";
const SANITIZED_BASH_SHEBANG: &str = "/usr/bin/env -S -u BASH_ENV -u BASHOPTS -u ENV -u SHELLOPTS /usr/bin/bash --noprofile --norc";

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedSource {
    path: String,
    mode: u32,
    sha256: String,
}

struct TrackedPath {
    path: String,
    mode: u32,
}

pub(in crate::structure::suppression::policy) fn validate(workspace: &Path) -> Result<()> {
    let tracked = tracked_paths(workspace)?;
    let untracked = untracked_paths(workspace)?;
    reject_unsupported_python_entrypoints(workspace, tracked.iter().map(|entry| entry.path.as_str()).chain(untracked.iter().map(String::as_str)))?;
    if let Some(path) = untracked.iter().find(|path| has_extension(path, "py")) {
        bail!("untracked Python source is outside every reviewed profile: {path:?}");
    }
    let observed = observed_sources(workspace, &tracked)?;
    if !tracked.iter().any(|entry| entry.path == POLICY_PATH) {
        bail!("Python source profile policy {POLICY_PATH:?} must remain tracked");
    }
    let policy = profile::load(workspace)?;
    revision::compare_previous(workspace, &policy)?;
    let observed_profile = profile_digest(&observed);
    if !policy.matches_current(&observed_profile) {
        bail!(
            "Python source tree does not match the current atomic profile: expected={}, observed={observed_profile}; preauthorize the complete source profile before changing Python files",
            policy.current_sha256
        );
    }
    Ok(())
}

fn profile_digest(observed: &[ObservedSource]) -> String {
    let mut digest = Sha256::new();
    for source in observed {
        digest.update(format!("{:06o} {} {}:{}\n", source.mode, source.sha256, source.path.len(), source.path));
    }
    format!("{:x}", digest.finalize())
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
    let ordinary = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-files", "-z", "--others", "--exclude-standard"])
        .output()
        .context("list untracked Python source profile inputs")?;
    if !ordinary.status.success() {
        bail!("git ls-files failed while listing untracked Python source profile inputs");
    }
    let mut paths = parse_nul_paths(&ordinary.stdout, |_| true)?;
    paths.extend(ignored_python_paths(workspace, &[":(top,icase,glob)**/*.py", ":(top,icase,glob)**/*.pyw"])?);
    paths.sort();
    paths.dedup();
    Ok(paths)
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

fn reject_unsupported_python_entrypoints<'a>(workspace: &Path, paths: impl Iterator<Item = &'a str>) -> Result<()> {
    for path in paths {
        if has_extension(path, "pyw") {
            bail!("Python .pyw execution sources are unsupported; use a reviewed .py source: {path:?}");
        }
        let absolute = workspace.join(path);
        if !has_extension(path, "py") && !is_regular_rust_source(path, &absolute)? && has_unsupported_shebang(&absolute)? {
            bail!("extensionless Python or dynamically selected interpreter sources are unsupported; use a reviewed .py source: {path:?}");
        }
    }
    Ok(())
}

fn is_regular_rust_source(path: &str, absolute: &Path) -> Result<bool> {
    if !has_extension(path, "rs") {
        return Ok(false);
    }
    match fs::symlink_metadata(absolute) {
        Ok(metadata) => Ok(metadata.is_file() && !metadata.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect possible Rust source {}", absolute.display())),
    }
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
    if metadata.file_type().is_symlink() {
        return Ok(true);
    }
    if !metadata.is_file() {
        return Ok(false);
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
    if interpreter == SANITIZED_BASH_SHEBANG {
        return false;
    }
    let words = interpreter.split_whitespace().collect::<Vec<_>>();
    let Some(command) = words.first() else {
        return false;
    };
    if command.starts_with('[') {
        return true;
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
    fn atomic_profile_digest_covers_path_mode_content_and_cardinality() {
        let source = ObservedSource {
            path: "script/check.py".to_owned(),
            mode: 0o100_644,
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        };
        let expected = profile_digest(std::slice::from_ref(&source));
        for changed in [
            ObservedSource {
                path: "script/renamed.py".to_owned(),
                ..source.clone()
            },
            ObservedSource {
                mode: 0o100_755,
                ..source.clone()
            },
            ObservedSource {
                sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
                ..source.clone()
            },
        ] {
            assert_ne!(profile_digest(&[changed]), expected);
        }
        assert_ne!(profile_digest(&[]), expected);
        assert_ne!(profile_digest(&[source.clone(), source]), expected);
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
        for (name, shebang) in [
            ("shell", "#!/bin/sh\n"),
            ("env-shell", "#!/usr/bin/env bash\n"),
            (
                "sanitized-env-shell",
                "#!/usr/bin/env -S -u BASH_ENV -u BASHOPTS -u ENV -u SHELLOPTS /usr/bin/bash --noprofile --norc\n",
            ),
        ] {
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

        let bracket_prefixed = workspace.path().join("bracket-prefixed");
        fs::write(&bracket_prefixed, "#![expect(missing_docs)]\n").expect("bracket-prefixed fixture");
        assert!(has_unsupported_shebang(&bracket_prefixed).expect("reject bracket-prefixed first line"));

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
    fn regular_rust_inner_attributes_are_not_treated_as_entrypoint_shebangs() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let rust_source = workspace.path().join("lib.rs");
        fs::write(&rust_source, "#![expect(missing_docs)]\npub fn value() {}\n").expect("Rust inner-attribute fixture");

        reject_unsupported_python_entrypoints(workspace.path(), ["lib.rs"].into_iter()).expect("regular Rust source");

        let extensionless = workspace.path().join("extensionless");
        fs::write(&extensionless, "#![expect(missing_docs)]\n").expect("extensionless bracket-prefixed fixture");
        let error = reject_unsupported_python_entrypoints(workspace.path(), ["extensionless"].into_iter()).unwrap_err();
        assert!(error.to_string().contains("unsupported"), "{error:#}");

        #[cfg(unix)]
        {
            let linked_source = workspace.path().join("linked.rs");
            std::os::unix::fs::symlink("lib.rs", &linked_source).expect("Rust source symlink fixture");
            let error = reject_unsupported_python_entrypoints(workspace.path(), ["linked.rs"].into_iter()).unwrap_err();
            assert!(error.to_string().contains("unsupported"), "{error:#}");
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

    #[test]
    fn ignored_python_source_is_rejected_before_profile_matching() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir_all(workspace.path().join("script")).expect("script directory");
        let status = std::process::Command::new("git")
            .current_dir(workspace.path())
            .args(["init", "--quiet"])
            .status()
            .expect("initialize repository");
        assert!(status.success());
        fs::write(
            workspace.path().join(".git/info/exclude"),
            "script/generated/\nscript/direct.pyw\nscript/target/\ntarget/\n.CACHE/\n",
        )
        .expect("local Git exclusion");
        fs::write(workspace.path().join("script/release.py"), "import json\n").expect("tracked Python importer");
        fs::create_dir_all(workspace.path().join("script/generated")).expect("ignored Python directory");
        fs::write(workspace.path().join("script/generated/json.PY"), "print('ignored shadow')\n").expect("ignored Python shadow");
        fs::write(workspace.path().join("script/direct.pyw"), "print('ignored entrypoint')\n").expect("ignored Python entrypoint");
        fs::create_dir_all(workspace.path().join("script/target")).expect("importable target-named directory");
        fs::write(workspace.path().join("script/target/json.py"), "print('importable target shadow')\n").expect("target-named Python shadow");
        fs::create_dir_all(workspace.path().join("target")).expect("build output directory");
        fs::write(workspace.path().join("target/generated.py"), "print('generated build output')\n").expect("generated Python build output");
        fs::create_dir_all(workspace.path().join(".CACHE")).expect("case-sensitive cache-named directory");
        fs::write(workspace.path().join(".CACHE/helper.py"), "print('case-sensitive cache source')\n").expect("case-sensitive cache source");
        let status = std::process::Command::new("git")
            .current_dir(workspace.path())
            .args(["add", "script/release.py"])
            .status()
            .expect("track Python importer");
        assert!(status.success());

        let untracked = untracked_paths(workspace.path()).expect("untracked Python inventory");
        assert!(untracked.contains(&"script/generated/json.PY".to_owned()), "{untracked:?}");
        assert!(untracked.contains(&"script/direct.pyw".to_owned()), "{untracked:?}");
        assert!(untracked.contains(&"script/target/json.py".to_owned()), "{untracked:?}");
        assert!(untracked.contains(&".CACHE/helper.py".to_owned()), "{untracked:?}");
        assert!(!untracked.contains(&"target/generated.py".to_owned()), "{untracked:?}");
        fs::remove_file(workspace.path().join("script/direct.pyw")).expect("remove ignored Python entrypoint fixture");
        let error = validate(workspace.path()).unwrap_err();
        assert!(error.to_string().contains("untracked Python source"), "{error:#}");
        assert!(error.to_string().contains(".CACHE/helper.py"), "{error:#}");

        let case_variant_workspace = tempfile::tempdir().expect("temporary case-variant workspace");
        let status = std::process::Command::new("git")
            .current_dir(case_variant_workspace.path())
            .args(["init", "--quiet"])
            .status()
            .expect("initialize case-variant repository");
        assert!(status.success());
        fs::write(case_variant_workspace.path().join(".git/info/exclude"), "TARGET/\n").expect("case-variant Git exclusion");
        fs::create_dir_all(case_variant_workspace.path().join("TARGET")).expect("case-variant target-named directory");
        fs::write(case_variant_workspace.path().join("TARGET/json.py"), "print('case-variant target shadow')\n").expect("case-variant target shadow");
        let untracked = untracked_paths(case_variant_workspace.path()).expect("case-variant Python inventory");
        assert!(untracked.contains(&"TARGET/json.py".to_owned()), "{untracked:?}");
    }
}
