use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::super::{parse_nul_paths, validate_relative_path};
use super::actions::validate_local_actions;
use super::arguments::execution_inputs_for_surface;
use super::is_execution_surface;

pub(super) struct ExecutionSurfaceSet {
    pub(super) paths: Vec<String>,
    pub(super) tracked_paths: BTreeSet<String>,
}

pub(super) fn execution_surfaces(workspace: &Path) -> Result<ExecutionSurfaceSet> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .output()
        .context("list checked-in command execution surfaces")?;
    if !output.status.success() {
        bail!("git ls-files failed while listing command execution surfaces");
    }
    let paths = parse_nul_paths(&output.stdout, |_| true)?.into_iter().collect::<BTreeSet<_>>();
    let tracked_paths = tracked_paths(workspace)?;
    let executables = tracked_executables(workspace)?;
    validate_local_actions(workspace, &paths)?;
    let mut surfaces = BTreeSet::new();
    for path in paths {
        let absolute = workspace.join(&path);
        let classified = is_execution_surface(&path) || executables.contains(&path);
        match fs::symlink_metadata(&absolute) {
            Ok(metadata) if metadata.is_dir() => continue,
            Ok(metadata) if metadata.file_type().is_symlink() && classified => bail!("command execution surface cannot be a symlink: {path:?}"),
            Ok(metadata) if metadata.file_type().is_symlink() => continue,
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("inspect possible command execution surface {}", absolute.display())),
        }
        if classified || has_shebang(&absolute)? {
            surfaces.insert(path);
        }
    }
    let mut pending = surfaces.iter().cloned().collect::<Vec<_>>();
    while let Some(surface) = pending.pop() {
        let source = fs::read_to_string(workspace.join(&surface)).with_context(|| format!("read command execution surface {surface}"))?;
        let reviewed_source = without_reviewed_protected_dispatch(&surface, &source);
        let (referenced_inputs, unresolved_input) = execution_inputs_for_surface(&surface, &reviewed_source);
        if unresolved_input {
            bail!("command execution surface {surface:?} uses an opaque interpreter program or makefile selection");
        }
        for input in referenced_inputs {
            let tracked = tracked_paths.contains(&input);
            if !tracked && reviewed_generated_program(&surface, &source, &input) {
                continue;
            }
            if !tracked {
                bail!("command execution surface {surface:?} references an execution input outside the tracked path inventory: {input:?}");
            }
            let absolute = workspace.join(&input);
            let metadata = fs::symlink_metadata(&absolute).with_context(|| format!("inspect execution input {}", absolute.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("execution input must be a regular non-symlink file: {input:?}");
            }
            if surfaces.insert(input.clone()) {
                pending.push(input);
            }
        }
    }
    Ok(ExecutionSurfaceSet {
        paths: surfaces.into_iter().collect(),
        tracked_paths,
    })
}

const TRUSTED_WORKFLOW: &str = ".github/workflows/trusted-maintainability.yml";
const TRUSTED_DISPATCH_LINE: &str = "          /usr/bin/bash \"$trusted_bootstrap\" --root \"$candidate_root\" --maintainability";
const TRUSTED_DISPATCH_AUTHENTICATION: &[&str] = &[
    "          trusted_root=$(/usr/bin/realpath -- ../.trusted-gate)",
    "          candidate_root=$(/usr/bin/realpath -- .)",
    "          if [[ \"$trusted_root\" != \"$workspace_root/.trusted-gate\" || \"$candidate_root\" != \"$workspace_root/.candidate\" ]]; then",
    "          trusted_bootstrap=\"$trusted_root/script/check-maintainability-bootstrap.sh\"",
    "          if [[ ! -f \"$trusted_bootstrap\" || -L \"$trusted_bootstrap\" ]]; then",
    TRUSTED_DISPATCH_LINE,
];

fn without_reviewed_protected_dispatch(surface: &str, source: &str) -> String {
    if surface != TRUSTED_WORKFLOW
        || !TRUSTED_DISPATCH_AUTHENTICATION
            .iter()
            .all(|expected| source.lines().filter(|line| line == expected).count() == 1)
    {
        return source.to_owned();
    }
    let protected_references_are_closed = source
        .lines()
        .filter(|line| line.contains("trusted_bootstrap"))
        .all(|line| TRUSTED_DISPATCH_AUTHENTICATION.contains(&line));
    if !protected_references_are_closed {
        return source.to_owned();
    }
    source
        .lines()
        .map(|line| if line == TRUSTED_DISPATCH_LINE { "          :" } else { line })
        .collect::<Vec<_>>()
        .join("\n")
}

fn reviewed_generated_program(surface: &str, source: &str, input: &str) -> bool {
    if input != "hold-smoke.exe" {
        return false;
    }
    let expected: &[&str] = match surface {
        ".github/workflows/release.yml" => &[
            "Copy-Item -LiteralPath $binary -Destination ./hold-smoke.exe",
            "$actual = ./hold-smoke.exe --version",
            "./hold-smoke.exe --help | Out-Null",
        ],
        ".github/workflows/release-smoke.yml" => &[
            "Copy-Item -LiteralPath $binary -Destination ./hold-smoke.exe",
            "$actualVersion = ./hold-smoke.exe --version",
            "./hold-smoke.exe --help | Out-Null",
            "$responseLines = @($request | ./hold-smoke.exe 2>\"$env:RUNNER_TEMP/mcp-stderr.log\")",
        ],
        _ => return false,
    };
    let observed = source.lines().map(str::trim).filter(|line| line.contains("hold-smoke.exe")).collect::<Vec<_>>();
    observed.len() == expected.len() && expected.iter().all(|line| observed.iter().filter(|candidate| *candidate == line).count() == 1)
}

fn tracked_paths(workspace: &Path) -> Result<BTreeSet<String>> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-files", "-z", "--cached"])
        .output()
        .context("list tracked command surfaces")?;
    if !output.status.success() {
        bail!("git ls-files failed while listing tracked command surfaces");
    }
    Ok(parse_nul_paths(&output.stdout, |_| true)?.into_iter().collect())
}

fn tracked_executables(workspace: &Path) -> Result<BTreeSet<String>> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-files", "-z", "--stage", "--cached"])
        .output()
        .context("list tracked executable command surfaces")?;
    if !output.status.success() {
        bail!("git ls-files failed while listing executable command surfaces");
    }
    let mut paths = BTreeSet::new();
    for record in output.stdout.split(|byte| *byte == b'\0').filter(|record| !record.is_empty()) {
        let record = std::str::from_utf8(record).context("tracked executable entry is not UTF-8")?;
        let (metadata, path) = record.split_once('\t').context("tracked executable entry has no path")?;
        validate_relative_path(path, "tracked executable path")?;
        if metadata.split_ascii_whitespace().next() == Some("100755") {
            paths.insert(path.to_owned());
        }
    }
    Ok(paths)
}

fn has_shebang(path: &Path) -> Result<bool> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("inspect possible command execution surface {}", path.display())),
    };
    if !metadata.is_file() {
        return Ok(false);
    }
    let mut prefix = [0_u8; 256];
    let mut file = fs::File::open(path).with_context(|| format!("inspect possible command execution surface {}", path.display()))?;
    let length = file
        .read(&mut prefix)
        .with_context(|| format!("read possible command execution surface {}", path.display()))?;
    let Some(directive) = prefix[..length].strip_prefix(b"#!") else {
        return Ok(false);
    };
    let directive = directive
        .split(|byte| matches!(byte, b'\n' | b'\r'))
        .next()
        .unwrap_or_default()
        .iter()
        .copied()
        .skip_while(u8::is_ascii_whitespace)
        .collect::<Vec<_>>();
    Ok(directive.first().is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.')))
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use std::process::Command;

    use super::{TRUSTED_DISPATCH_AUTHENTICATION, TRUSTED_DISPATCH_LINE, TRUSTED_WORKFLOW, execution_surfaces, without_reviewed_protected_dispatch};

    #[test]
    fn protected_dispatch_requires_the_complete_canonical_authentication_sequence() {
        let reviewed = TRUSTED_DISPATCH_AUTHENTICATION.join("\n");
        let sanitized = without_reviewed_protected_dispatch(TRUSTED_WORKFLOW, &reviewed);
        assert!(!sanitized.contains(TRUSTED_DISPATCH_LINE));
        assert!(sanitized.lines().any(|line| line.trim() == ":"));

        for changed in [
            reviewed.replacen("../.trusted-gate", "../.candidate", 1),
            reviewed.replacen("--maintainability", "--test-environment", 1),
            format!("{reviewed}\n          printf '%s\\n' \"$trusted_bootstrap\""),
        ] {
            assert_eq!(without_reviewed_protected_dispatch(TRUSTED_WORKFLOW, &changed), changed);
        }
    }

    #[test]
    fn unrelated_symlinks_are_not_command_surfaces() {
        let repository = tempfile::tempdir().expect("temporary repository");
        fs::create_dir(repository.path().join("docs")).expect("create docs directory");
        fs::create_dir(repository.path().join("script")).expect("create script directory");
        fs::create_dir(repository.path().join("src")).expect("create source directory");
        fs::write(repository.path().join("docs/guide.md"), "guide\n").expect("write guide");
        fs::write(repository.path().join("src/lib.rs"), "#![expect(missing_docs)]\npub fn value() {}\n").expect("write Rust inner attribute");
        symlink("guide.md", repository.path().join("docs/latest.md")).expect("create documentation symlink");
        fs::write(repository.path().join("script/check.sh"), "#!/bin/sh\nexit 0\n").expect("write command surface");
        git(repository.path(), &["init", "--quiet"]);
        git(repository.path(), &["add", "."]);

        let surfaces = execution_surfaces(repository.path()).expect("classify execution surfaces");
        assert!(surfaces.paths.contains(&"script/check.sh".to_owned()));
        assert!(!surfaces.paths.contains(&"docs/latest.md".to_owned()));
        assert!(!surfaces.paths.contains(&"src/lib.rs".to_owned()));

        symlink("check.sh", repository.path().join("script/linked.sh")).expect("create command symlink");
        git(repository.path(), &["add", "script/linked.sh"]);
        let error = execution_surfaces(repository.path()).err().expect("reject command surface symlink");
        assert!(error.to_string().contains("command execution surface cannot be a symlink"));
    }

    #[test]
    fn generated_release_smoke_programs_have_a_closed_invocation_set() {
        let repository = tempfile::tempdir().expect("temporary repository");
        fs::create_dir_all(repository.path().join(".github/workflows")).expect("create workflow directory");
        let workflow = repository.path().join(".github/workflows/release.yml");
        let reviewed = r"name: release
jobs:
  smoke:
    runs-on: windows-latest
    steps:
      - shell: pwsh
        run: |
          Copy-Item -LiteralPath $binary -Destination ./hold-smoke.exe
          $actual = ./hold-smoke.exe --version
          ./hold-smoke.exe --help | Out-Null
";
        fs::write(&workflow, reviewed).expect("write reviewed workflow");
        git(repository.path(), &["init", "--quiet"]);
        git(repository.path(), &["add", "."]);
        execution_surfaces(repository.path()).expect("accept exact generated smoke program");

        fs::write(&workflow, format!("{reviewed}          ./hold-smoke.exe --version\n")).expect("add unreviewed invocation");
        let error = execution_surfaces(repository.path()).err().expect("reject added invocation");
        assert!(error.to_string().contains("outside the tracked path inventory"));
    }

    fn git(repository: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .current_dir(repository)
            .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"])
            .args(arguments)
            .status()
            .expect("run git");
        assert!(status.success(), "git {arguments:?}");
    }
}
