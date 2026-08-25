use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::super::{ignored_python_paths, parse_nul_paths, validate_relative_path};
use super::actions::validate_local_actions;
#[cfg(all(test, unix))]
use super::arguments::execution_inputs_for_surface;
use super::arguments::{WorkspaceAnalyzer, WorkspaceContext, execution_inputs_for_surface_in_workspace};
use super::is_execution_surface;
use super::profile_policy::ProfileManifest;

pub(super) struct ExecutionSurfaceSet {
    pub(super) paths: Vec<String>,
    pub(super) checked_paths: BTreeSet<String>,
    pub(super) tracked_paths: BTreeSet<String>,
    pub(super) command_profiles: Option<ProfileManifest>,
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
    let mut paths = parse_nul_paths(&output.stdout, |_| true)?.into_iter().collect::<BTreeSet<_>>();
    paths.extend(ignored_python_paths(
        workspace,
        &[
            ":(top,icase,glob)**/*.pyc",
            ":(top,icase,glob)**/*.pyo",
            ":(top,icase,glob)**/*.pth",
            ":(top,icase,glob)**/*.pyd",
            ":(top,icase,glob)**/*.so",
            ":(top,icase,glob)**/*.zip",
            ":(top,icase,glob)**/*.egg",
            ":(top,icase,glob)**/*.whl",
            ":(top,icase,glob)**/*.pyz",
            ":(top,icase,glob)**/*.pyzw",
            ":(top,icase,glob)**/__pycache__/**",
            ":(top,icase).pdbrc",
            ":(top,icase,glob)**/.pdbrc",
        ],
    )?);
    let checked_paths = paths.clone();
    let mut shadow_paths = checked_paths.clone();
    shadow_paths.extend(ignored_python_paths(workspace, &[":(top,glob)*"])?.into_iter().filter(|path| !path.contains('/')));
    let tracked_paths = tracked_paths(workspace)?;
    let command_profiles = super::command_profiles::validate(workspace, &tracked_paths)?;
    let executables = tracked_executables(workspace)?;
    validate_local_actions(workspace, &paths)?;
    let mut surfaces = discover_surfaces(workspace, paths, &executables)?;
    for surface in &surfaces {
        validate_before_resolution(workspace, surface)?;
    }
    close_over_execution_inputs(workspace, &mut surfaces, &shadow_paths, &tracked_paths, command_profiles.as_ref())?;
    Ok(ExecutionSurfaceSet {
        paths: surfaces.into_iter().collect(),
        checked_paths,
        tracked_paths,
        command_profiles,
    })
}

fn discover_surfaces(workspace: &Path, paths: BTreeSet<String>, executables: &BTreeSet<String>) -> Result<BTreeSet<String>> {
    let mut surfaces = BTreeSet::new();
    for path in paths {
        reject_python_loadable_artifact(&path)?;
        let absolute = workspace.join(&path);
        let executable = executables.contains(&path);
        let classified = is_execution_surface(&path) || executable;
        match fs::symlink_metadata(&absolute) {
            Ok(metadata) if metadata.is_dir() => continue,
            Ok(metadata) if metadata.file_type().is_symlink() && classified => bail!("command execution surface cannot be a symlink: {path:?}"),
            Ok(metadata) if metadata.file_type().is_symlink() => continue,
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("inspect possible command execution surface {}", absolute.display())),
        }
        let shebang = has_shebang(&absolute)?;
        if is_rust_source(&path) {
            if executable || shebang {
                bail!("tracked Rust source cannot be executable or have a shebang: {path:?}");
            }
            continue;
        }
        if classified || shebang {
            surfaces.insert(path);
        }
    }
    Ok(surfaces)
}

fn close_over_execution_inputs(
    workspace: &Path,
    surfaces: &mut BTreeSet<String>,
    checked_paths: &BTreeSet<String>,
    tracked_paths: &BTreeSet<String>,
    command_profiles: Option<&ProfileManifest>,
) -> Result<()> {
    let mut validated = surfaces.clone();
    let mut pending = surfaces.iter().cloned().collect::<Vec<_>>();
    let mut windows_bare_programs = BTreeSet::new();
    while let Some(surface) = pending.pop() {
        let source = fs::read_to_string(workspace.join(&surface)).with_context(|| format!("read command execution surface {surface}"))?;
        let source_is_reviewed = command_profiles.is_some_and(|profiles| profiles.source_is_current(&surface, &source));
        let reviewed_source = without_reviewed_dispatch(&surface, &source, source_is_reviewed);
        let execution_inputs = execution_inputs_for_surface_in_workspace(
            WorkspaceContext {
                root: workspace,
                execution_surfaces: surfaces,
                tracked_paths,
            },
            &surface,
            &reviewed_source,
            source_is_reviewed,
        );
        windows_bare_programs.extend(execution_inputs.windows_bare_programs.iter().cloned());
        let mut referenced_inputs = execution_inputs.paths;
        referenced_inputs.extend(windows_bare_program_shadows(&execution_inputs.windows_bare_programs, checked_paths));
        if execution_inputs.unresolved {
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
            if validated.insert(input.clone()) {
                validate_before_resolution(workspace, &input)?;
            }
            if surfaces.insert(input.clone()) {
                pending.push(input);
            }
        }
    }
    let mut protected_surfaces = surfaces.clone();
    protected_surfaces.extend(windows_bare_program_candidates(&windows_bare_programs));
    let analyzer = WorkspaceAnalyzer::new(WorkspaceContext {
        root: workspace,
        execution_surfaces: &protected_surfaces,
        tracked_paths,
    });
    for surface in surfaces.iter() {
        let source = fs::read_to_string(workspace.join(surface)).with_context(|| format!("read command execution surface {surface}"))?;
        let source_is_reviewed = command_profiles.is_some_and(|profiles| profiles.source_is_current(surface, &source));
        let reviewed_source = without_reviewed_dispatch(surface, &source, source_is_reviewed);
        let execution_inputs = analyzer.execution_inputs(surface, &reviewed_source, source_is_reviewed);
        if execution_inputs.unresolved {
            bail!("command execution surface {surface:?} uses an opaque interpreter program or makefile selection");
        }
    }
    Ok(())
}

fn validate_before_resolution(workspace: &Path, surface: &str) -> Result<()> {
    let source = fs::read_to_string(workspace.join(surface)).with_context(|| format!("read command execution surface {surface}"))?;
    super::validate_before_resolution(workspace, surface, &source)
}

fn reject_python_loadable_artifact(path: &str) -> Result<()> {
    let path = Path::new(path);
    let debugger_commands = path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.eq_ignore_ascii_case(".pdbrc"));
    let in_bytecode_cache = path
        .components()
        .any(|component| component.as_os_str().to_str().is_some_and(|component| component.eq_ignore_ascii_case("__pycache__")));
    let loadable_extension = path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "pyc" | "pyo" | "pth" | "pyd" | "so" | "zip" | "egg" | "whl" | "pyz" | "pyzw"
        )
    });
    if debugger_commands || in_bytecode_cache || loadable_extension {
        bail!(
            "Python-loadable artifact is unsupported because its executable contents cannot be audited: {}",
            path.display()
        );
    }
    Ok(())
}

fn is_rust_source(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

fn windows_bare_program_shadows(programs: &BTreeSet<String>, checked_paths: &BTreeSet<String>) -> BTreeSet<String> {
    let candidates = windows_bare_program_candidates(programs);
    checked_paths
        .iter()
        .filter(|path| !path.contains('/') && candidates.contains(&path.to_ascii_lowercase()))
        .cloned()
        .collect()
}

fn windows_bare_program_candidates(programs: &BTreeSet<String>) -> BTreeSet<String> {
    const WINDOWS_EXECUTABLE_SUFFIXES: &[&str] = &["", ".exe", ".com", ".bat", ".cmd"];
    programs
        .iter()
        .flat_map(|program| WINDOWS_EXECUTABLE_SUFFIXES.iter().map(move |suffix| format!("{program}{suffix}")))
        .collect()
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
        prefix_sha256: "c4f9dce7fc27994bfe0f719ba4f62a6daeb7ddec28fafefba5e9edf1e952aa8b",
    },
    TrustedDispatchJob {
        header: "  trusted-dependency-unsafe-windows:",
        runner: "    runs-on: windows-latest",
        authentication: TRUSTED_WINDOWS_DEPENDENCY_AUTHENTICATION,
        prefix_sha256: "9a2c1d5e70da60f9267108ddcf387a46ba5c17f19c10195a4509e73db54d4814",
    },
];

pub(in crate::structure::suppression::policy::config) fn without_reviewed_dispatch(surface: &str, source: &str, source_is_reviewed: bool) -> String {
    let mut source = without_reviewed_protected_dispatch(surface, source);
    if super::reviewed_bootstrap_reexec_is_exact(surface, &source, source_is_reviewed) {
        source = without_reviewed_bootstrap_reexec(&source);
    }
    let reviewed_lines = match surface {
        "script/install.sh" if super::reviewed_quality_command_exceptions_are_exact(surface, &source, source_is_reviewed) => super::INSTALL_COMMAND_LINES,
        "script/tests/test_maintainability_bootstrap.sh"
            if source_is_reviewed
                && super::BOOTSTRAP_TEST_OPAQUE_COMMAND_LINES
                    .iter()
                    .all(|expected| source.lines().filter(|line| line == expected).count() == 1) =>
        {
            super::BOOTSTRAP_TEST_OPAQUE_COMMAND_LINES
        }
        _ => return source,
    };
    source
        .lines()
        .map(|line| {
            if reviewed_lines.contains(&line) {
                format!("{}:", &line[..line.len() - line.trim_start().len()])
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(in crate::structure::suppression::policy::config::command) fn without_reviewed_bootstrap_reexec(source: &str) -> String {
    const START: &str = "scrub_untrusted_environment() {";
    const END: &str = "\nscrub_untrusted_environment \"$@\"";
    let start = source.find(START).expect("reviewed bootstrap scrub start");
    let end = source[start..].find(END).expect("reviewed bootstrap scrub end") + start + END.len();
    format!("{}:{}", &source[..start], &source[end..])
}

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
mod tests;
