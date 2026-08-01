use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

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
const TRUSTED_MAINTAINABILITY_DISPATCH_LINE: &str = "          /usr/bin/bash \"$trusted_bootstrap\" --root \"$candidate_root\" --maintainability";
const TRUSTED_MAINTAINABILITY_AUTHENTICATION: &[&str] = &[
    "          workspace_root=$(/usr/bin/realpath -- \"$GITHUB_WORKSPACE\")",
    "          trusted_root=$(/usr/bin/realpath -- ../.trusted-gate)",
    "          candidate_root=$(/usr/bin/realpath -- .)",
    "          if [[ \"$trusted_root\" != \"$workspace_root/.trusted-gate\" || \"$candidate_root\" != \"$workspace_root/.candidate\" ]]; then",
    "          readonly workspace_root trusted_root candidate_root",
    "          trusted_bootstrap=\"$trusted_root/script/check-maintainability-bootstrap.sh\"",
    "          if [[ ! -f \"$trusted_bootstrap\" || -L \"$trusted_bootstrap\" ]]; then",
    "          readonly trusted_bootstrap",
    TRUSTED_MAINTAINABILITY_DISPATCH_LINE,
];
const TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE: &str = "          /usr/bin/bash \"$protected_bootstrap\" --root \"$audit_root\" --dependency-unsafe";
const TRUSTED_WINDOWS_DEPENDENCY_AUTHENTICATION: &[&str] = &[
    "          workspace_root=$(/usr/bin/cygpath -u -- \"$GITHUB_WORKSPACE\")",
    "          workspace_root=$(/usr/bin/realpath -- \"$workspace_root\")",
    "          protected_root=$(/usr/bin/realpath -- ../.trusted-gate)",
    "          audit_root=$(/usr/bin/realpath -- .)",
    "          if [[ \"$protected_root\" != \"$workspace_root/.trusted-gate\" || \"$audit_root\" != \"$workspace_root/.candidate\" ]]; then",
    "          readonly workspace_root protected_root audit_root",
    "            windows_base_revision=$(git rev-parse --verify \"${remote_ref}^{commit}\")",
    "          protected_bootstrap=\"$protected_root/script/check-maintainability-bootstrap.sh\"",
    "          if [[ ! -f \"$protected_bootstrap\" || -L \"$protected_bootstrap\" ]]; then",
    "          readonly protected_bootstrap",
    TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE,
];
const PROTECTED_DISPATCH_REFERENCES: &[&str] = &[
    "workspace_root",
    "trusted_root",
    "candidate_root",
    "trusted_bootstrap",
    "protected_root",
    "audit_root",
    "protected_bootstrap",
];
struct TrustedDispatchJob {
    header: &'static str,
    runner: &'static str,
    authentication: &'static [&'static str],
    // Pins every job line through the protected dispatch so no candidate command can run first.
    prefix_sha256: &'static str,
}

const TRUSTED_DISPATCH_JOBS: &[TrustedDispatchJob] = &[
    TrustedDispatchJob {
        header: "  trusted-maintainability:",
        runner: "    runs-on: ubuntu-latest",
        authentication: TRUSTED_MAINTAINABILITY_AUTHENTICATION,
        prefix_sha256: "bb454c630156aeb850793b7967779558e95f9724c53c3afb947ccfdeefb4b0f6",
    },
    TrustedDispatchJob {
        header: "  trusted-dependency-unsafe-windows:",
        runner: "    runs-on: windows-latest",
        authentication: TRUSTED_WINDOWS_DEPENDENCY_AUTHENTICATION,
        prefix_sha256: "4689ac5f093df5c90c713cede6584ea917eda2bf82a31b4345255ae16ee647a0",
    },
];

fn without_reviewed_protected_dispatch(surface: &str, source: &str) -> String {
    if surface != TRUSTED_WORKFLOW || !TRUSTED_DISPATCH_JOBS.iter().all(|job| reviewed_dispatch_job(source, job)) {
        return source.to_owned();
    }
    let protected_references_are_closed = source
        .lines()
        .filter(|line| PROTECTED_DISPATCH_REFERENCES.iter().any(|reference| line.contains(reference)))
        .all(|line| TRUSTED_DISPATCH_JOBS.iter().any(|job| job.authentication.contains(&line)));
    if !protected_references_are_closed {
        return source.to_owned();
    }
    source
        .lines()
        .map(|line| {
            if matches!(line, TRUSTED_MAINTAINABILITY_DISPATCH_LINE | TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE) {
                "          :"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn reviewed_dispatch_job(source: &str, job: &TrustedDispatchJob) -> bool {
    let lines = source.lines().collect::<Vec<_>>();
    let mut starts = lines.iter().enumerate().filter(|(_, line)| **line == job.header).map(|(index, _)| index);
    let Some(start) = starts.next() else {
        return false;
    };
    if starts.next().is_some() {
        return false;
    }
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.starts_with("  ") && !line.starts_with("    ") && !line.trim().is_empty())
        .map_or(lines.len(), |offset| start + 1 + offset);
    let body = &lines[start..end];
    if body.iter().filter(|line| **line == job.runner).count() != 1 {
        return false;
    }
    let mut after = 0;
    for expected in job.authentication {
        let matches = body.iter().enumerate().filter(|(_, line)| *line == expected).map(|(index, _)| index).collect::<Vec<_>>();
        if matches.len() != 1 || matches[0] < after {
            return false;
        }
        after = matches[0] + 1;
    }
    let Some(dispatch) = job.authentication.last().and_then(|line| body.iter().position(|candidate| candidate == line)) else {
        return false;
    };
    let prefix = body[..=dispatch].join("\n");
    format!("{:x}", Sha256::digest(prefix.as_bytes())) == job.prefix_sha256
}

fn reviewed_generated_program(surface: &str, source: &str, input: &str) -> bool {
    if input != "hold-smoke.exe" {
        return false;
    }
    let expected: &[&str] = match surface {
        ".github/workflows/ci.yml" => &[
            "Copy-Item -LiteralPath $binary -Destination ./hold-smoke.exe",
            "./hold-smoke.exe --version",
            "./hold-smoke.exe --help | Out-Null",
        ],
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

    use super::{
        TRUSTED_MAINTAINABILITY_AUTHENTICATION, TRUSTED_MAINTAINABILITY_DISPATCH_LINE, TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE, TRUSTED_WORKFLOW, execution_surfaces,
        without_reviewed_protected_dispatch,
    };

    #[test]
    fn protected_dispatch_requires_the_complete_canonical_authentication_sequence() {
        let reviewed = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.github/workflows/trusted-maintainability.yml"));
        let sanitized = without_reviewed_protected_dispatch(TRUSTED_WORKFLOW, reviewed);
        assert!(!sanitized.contains(TRUSTED_MAINTAINABILITY_DISPATCH_LINE));
        assert!(!sanitized.contains(TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE));
        assert_eq!(sanitized.lines().filter(|line| line.trim() == ":").count(), 2);

        for changed in [
            reviewed.replacen("../.trusted-gate", "../.candidate", 1),
            reviewed.replacen("--maintainability", "--test-environment", 1),
            reviewed.replacen("/usr/bin/cygpath -u -- \"$GITHUB_WORKSPACE\"", "printf '%s' \"$GITHUB_WORKSPACE\"", 1),
            reviewed.replacen("$(git rev-parse", "$(/usr/bin/git rev-parse", 1),
            reviewed.replacen("$protected_root", "$audit_root", 1),
            reviewed.replacen("--dependency-unsafe", "--test-environment", 1),
            reviewed.replacen("runs-on: windows-latest", "runs-on: ubuntu-latest", 1),
            format!("{reviewed}\n          printf '%s\\n' \"$trusted_bootstrap\""),
            format!("{reviewed}\n          printf '%s\\n' \"$protected_bootstrap\""),
            reviewed.replacen(
                TRUSTED_MAINTAINABILITY_DISPATCH_LINE,
                &format!("          candidate_root=$workspace_root/decoy\n{TRUSTED_MAINTAINABILITY_DISPATCH_LINE}"),
                1,
            ),
            reviewed.replacen(
                TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE,
                &format!("          audit_root=$workspace_root/decoy\n{TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE}"),
                1,
            ),
            reviewed.replacen(
                &format!("{}\n{}", TRUSTED_MAINTAINABILITY_AUTHENTICATION[0], TRUSTED_MAINTAINABILITY_AUTHENTICATION[1]),
                &format!("{}\n{}", TRUSTED_MAINTAINABILITY_AUTHENTICATION[1], TRUSTED_MAINTAINABILITY_AUTHENTICATION[0]),
                1,
            ),
            reviewed.replacen(
                TRUSTED_MAINTAINABILITY_DISPATCH_LINE,
                &format!("          cargo test --workspace\n{TRUSTED_MAINTAINABILITY_DISPATCH_LINE}"),
                1,
            ),
            reviewed.replacen(
                TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE,
                &format!("          cargo test --workspace\n{TRUSTED_WINDOWS_DEPENDENCY_DISPATCH_LINE}"),
                1,
            ),
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

    #[test]
    fn generated_ci_smoke_program_has_a_closed_invocation_set() {
        let reviewed = r"Copy-Item -LiteralPath $binary -Destination ./hold-smoke.exe
./hold-smoke.exe --version
./hold-smoke.exe --help | Out-Null
";
        assert!(super::reviewed_generated_program(".github/workflows/ci.yml", reviewed, "hold-smoke.exe"));
        assert!(!super::reviewed_generated_program(
            ".github/workflows/ci.yml",
            &format!("{reviewed}./hold-smoke.exe --version\n"),
            "hold-smoke.exe"
        ));
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
