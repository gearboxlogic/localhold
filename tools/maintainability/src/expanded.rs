use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::scan::{SiteKind, UnsafeSite};

use self::dep_info::{collect as collect_dep_info, verify as verify_dep_info};

const AUDITED_TARGET_KINDS: &[&str] = &["lib", "bin", "test", "bench", "example"];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Diagnostic {
    target: String,
    code: String,
    path: String,
    line: usize,
    column: usize,
    end_line: usize,
    end_column: usize,
    message: String,
}

#[derive(Default)]
struct AuditOutput {
    diagnostics: Vec<Diagnostic>,
    dep_info: BTreeSet<PathBuf>,
    root_artifacts: usize,
}

struct AuditLane<'a> {
    label: &'a str,
    cargo_args: &'a [&'a str],
    target_kinds: &'a [&'a str],
}

pub fn verify(workspace: &Path, sites: &[UnsafeSite], cargo_metadata: &[u8]) -> Result<()> {
    let workspace = fs::canonicalize(workspace).with_context(|| format!("resolve workspace {}", workspace.display()))?;
    let manifest_path = workspace.join("Cargo.toml");
    let manifest = fs::canonicalize(&manifest_path).with_context(|| format!("resolve workspace manifest {}", manifest_path.display()))?;
    let target_kinds = root_target_kinds(&manifest, cargo_metadata)?;
    let mut dep_info = BTreeSet::new();
    let mut root_artifacts = 0;
    let mut normal_diagnostics = Vec::new();
    for lane in [
        AuditLane {
            label: "normal",
            cargo_args: &["--lib", "--bins"],
            target_kinds: &["lib", "bin"],
        },
        AuditLane {
            label: "unit-test",
            cargo_args: &["--lib", "--tests"],
            target_kinds: AUDITED_TARGET_KINDS,
        },
        AuditLane {
            label: "integration-test",
            cargo_args: &["--tests"],
            target_kinds: &["test"],
        },
        AuditLane {
            label: "benchmark",
            cargo_args: &["--benches"],
            target_kinds: &["bench"],
        },
    ] {
        let optional_kind = match lane.label {
            "integration-test" => Some("test"),
            "benchmark" => Some("bench"),
            _ => None,
        };
        if let Some(kind) = optional_kind
            && !target_kinds.contains(kind)
        {
            continue;
        }
        let audit = run_audit_lane(&workspace, &manifest, &lane)?;
        let diagnostics = if lane.label == "unit-test" {
            subtract_diagnostics(&audit.diagnostics, &normal_diagnostics)
        } else {
            audit.diagnostics.clone()
        };
        compare_target_diagnostics(sites, &diagnostics)?;
        if lane.label == "normal" {
            normal_diagnostics.clone_from(&audit.diagnostics);
        }
        dep_info.extend(audit.dep_info);
        root_artifacts += audit.root_artifacts;
    }
    if target_kinds.contains("example") {
        let audit = run_audit_lane(
            &workspace,
            &manifest,
            &AuditLane {
                label: "example",
                cargo_args: &["--examples"],
                target_kinds: &["example"],
            },
        )?;
        compare_target_diagnostics(sites, &audit.diagnostics)?;
        dep_info.extend(audit.dep_info);
        root_artifacts += audit.root_artifacts;
    }
    if root_artifacts == 0 {
        bail!("compiler-expanded source audit observed no root-package artifacts");
    }
    if dep_info.is_empty() {
        bail!("compiler-expanded source audit found no root-package dep-info files");
    }
    verify_dep_info(&workspace, &dep_info)
}

fn root_target_kinds(manifest: &Path, cargo_metadata: &[u8]) -> Result<BTreeSet<String>> {
    let metadata: CargoMetadata = serde_json::from_slice(cargo_metadata).context("parse Cargo metadata for compiler-audit targets")?;
    let mut root_packages = Vec::new();
    for package in &metadata.packages {
        let package_manifest = fs::canonicalize(&package.manifest_path).with_context(|| format!("resolve Cargo metadata manifest {}", package.manifest_path.display()))?;
        if package_manifest == manifest {
            root_packages.push(package);
        }
    }
    let [root_package] = root_packages.as_slice() else {
        bail!(
            "Cargo metadata must contain exactly one root package for compiler-audit targets, found {}",
            root_packages.len()
        );
    };
    let target_kinds: BTreeSet<_> = root_package.targets.iter().flat_map(|target| target.kind.iter().cloned()).collect();
    if let Some(kind) = target_kinds.iter().find(|kind| !AUDITED_TARGET_KINDS.contains(&kind.as_str())) {
        bail!("Cargo metadata contains unsupported root target kind {kind:?}");
    }
    Ok(target_kinds)
}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Deserialize)]
struct CargoPackage {
    manifest_path: PathBuf,
    targets: Vec<CargoTarget>,
}

#[derive(Deserialize)]
struct CargoTarget {
    kind: Vec<String>,
}

fn run_audit_lane(workspace: &Path, manifest: &Path, lane: &AuditLane<'_>) -> Result<AuditOutput> {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(workspace)
        .arg("clippy")
        .args(lane.cargo_args)
        .args(["--all-features", "--locked", "--message-format=json", "--"])
        .args([
            "--force-warn=unsafe-code",
            "--force-warn=unsafe-op-in-unsafe-fn",
            "--force-warn=clippy::undocumented-unsafe-blocks",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    sanitize_compiler_environment(&mut command);

    let mut child = command.spawn().with_context(|| format!("start compiler-expanded {} audit", lane.label))?;
    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("compiler-expanded {} audit stdout was not piped", lane.label))?;
    let mut audit = AuditOutput::default();
    let parse_result = parse_cargo_output(BufReader::new(stdout), workspace, manifest, lane, &mut audit);
    let wait_result = child.wait().with_context(|| format!("wait for compiler-expanded {} audit", lane.label));
    parse_result?;
    let status = wait_result?;
    if !status.success() {
        bail!("compiler-expanded {} audit failed with {status}", lane.label);
    }
    if audit.root_artifacts == 0 {
        bail!("compiler-expanded {} audit observed no selected root-package artifacts", lane.label);
    }
    if audit.dep_info.is_empty() {
        bail!("compiler-expanded {} audit found no selected root-package dep-info files", lane.label);
    }
    Ok(audit)
}

fn parse_cargo_output<R: BufRead>(reader: R, workspace: &Path, manifest: &Path, lane: &AuditLane<'_>, audit: &mut AuditOutput) -> Result<()> {
    let mut parse_result = Ok(());
    for json_line in reader.lines() {
        let json_line = match json_line {
            Ok(json_line) => json_line,
            Err(error) => {
                if parse_result.is_ok() {
                    parse_result = Err(error).with_context(|| format!("read compiler-expanded {} audit output", lane.label));
                }
                break;
            }
        };
        if parse_result.is_err() || json_line.is_empty() {
            continue;
        }
        parse_result = (|| {
            let message: Value = serde_json::from_str(&json_line).with_context(|| format!("parse Cargo JSON from compiler-expanded {} audit", lane.label))?;
            handle_cargo_message(workspace, manifest, lane.target_kinds, &message, audit)
        })();
    }
    parse_result
}

fn handle_cargo_message(workspace: &Path, manifest: &Path, target_kinds: &[&str], message: &Value, audit: &mut AuditOutput) -> Result<()> {
    if !is_root_manifest(manifest, message)? {
        return Ok(());
    }
    let kinds = message
        .pointer("/target/kind")
        .and_then(Value::as_array)
        .context("root Cargo message is missing target kinds")?;
    if !kinds.iter().filter_map(Value::as_str).any(|kind| target_kinds.contains(&kind)) {
        return Ok(());
    }
    match message.get("reason").and_then(Value::as_str) {
        Some("compiler-message") => {
            audit.diagnostics.extend(parse_diagnostic(workspace, message)?);
        }
        Some("compiler-artifact") => {
            audit.root_artifacts += 1;
            collect_dep_info(message, &mut audit.dep_info)?;
        }
        _ => {}
    }
    Ok(())
}

fn is_root_manifest(manifest: &Path, message: &Value) -> Result<bool> {
    let Some(reported) = message.get("manifest_path").and_then(Value::as_str) else {
        return Ok(false);
    };
    let reported = fs::canonicalize(reported).with_context(|| format!("resolve Cargo message manifest {reported}"))?;
    Ok(reported == manifest)
}

pub fn sanitize_compiler_environment(command: &mut Command) {
    for (name, _) in env::vars_os() {
        if audit_environment_override(&name) {
            command.env_remove(name);
        }
    }
}

fn audit_environment_override(name: &OsStr) -> bool {
    name.to_str().map(str::to_ascii_uppercase).is_some_and(|name| {
        matches!(
            name.as_str(),
            "RUSTFLAGS"
                | "CARGO_ENCODED_RUSTFLAGS"
                | "CARGO_BUILD_TARGET"
                | "CLIPPY_ARGS"
                | "RUSTC"
                | "RUSTC_WRAPPER"
                | "RUSTC_WORKSPACE_WRAPPER"
                | "CARGO_BUILD_RUSTFLAGS"
                | "CARGO_BUILD_RUSTC"
                | "CARGO_BUILD_RUSTC_WRAPPER"
                | "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"
        ) || name.starts_with("CARGO_ALIAS_")
            || name.starts_with("CARGO_TARGET_") && name.ends_with("_RUSTFLAGS")
    })
}

fn parse_diagnostic(workspace: &Path, cargo_message: &Value) -> Result<Option<Diagnostic>> {
    let message = cargo_message.get("message").context("compiler message is missing message payload")?;
    let Some(raw_code) = message.pointer("/code/code").and_then(Value::as_str) else {
        return Ok(None);
    };
    let code = match raw_code {
        "unsafe_code" => "unsafe_code",
        "E0133" | "unsafe_op_in_unsafe_fn" => "unsafe_op_in_unsafe_fn",
        "clippy::undocumented_unsafe_blocks" => "clippy::undocumented_unsafe_blocks",
        _ => return Ok(None),
    };
    let primary: Vec<_> = message
        .get("spans")
        .and_then(Value::as_array)
        .context("safety diagnostic is missing spans")?
        .iter()
        .filter(|span| span.get("is_primary").and_then(Value::as_bool) == Some(true))
        .collect();
    let [span] = primary.as_slice() else {
        bail!("safety diagnostic must have exactly one primary span, found {}", primary.len());
    };
    let file_name = span
        .get("file_name")
        .and_then(Value::as_str)
        .context("safety diagnostic primary span is missing file_name")?;
    Ok(Some(Diagnostic {
        target: target_identity(cargo_message)?,
        code: code.to_owned(),
        path: workspace_relative_path(workspace, Path::new(file_name))?,
        line: json_usize(span, "line_start")?,
        column: json_usize(span, "column_start")?,
        end_line: json_usize(span, "line_end")?,
        end_column: json_usize(span, "column_end")?,
        message: message.get("message").and_then(Value::as_str).context("safety diagnostic is missing message")?.to_owned(),
    }))
}

fn target_identity(message: &Value) -> Result<String> {
    let target = message.get("target").context("root Cargo message is missing target")?;
    let name = target.get("name").and_then(Value::as_str).context("root Cargo target is missing name")?;
    let source = target.get("src_path").and_then(Value::as_str).context("root Cargo target is missing src_path")?;
    let kinds = target
        .get("kind")
        .and_then(Value::as_array)
        .context("root Cargo target is missing kind")?
        .iter()
        .map(|kind| kind.as_str().context("root Cargo target kind is not a string"))
        .collect::<Result<Vec<_>>>()?;
    Ok(format!("{name}:{}:{source}", kinds.join(",")))
}

fn json_usize(value: &Value, field: &str) -> Result<usize> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .with_context(|| format!("diagnostic span is missing valid {field}"))
}

fn workspace_relative_path(workspace: &Path, path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { workspace.join(path) };
    let canonical = fs::canonicalize(&absolute).with_context(|| format!("resolve compiler input {}", absolute.display()))?;
    let relative = canonical
        .strip_prefix(workspace)
        .with_context(|| format!("compiler input escaped audited workspace: {}", canonical.display()))?;
    Ok(relative.to_str().context("compiler input path is not UTF-8")?.replace('\\', "/"))
}

fn compare_diagnostics(sites: &[UnsafeSite], diagnostics: &[Diagnostic]) -> Result<()> {
    let mut diagnostics = diagnostics.to_vec();
    diagnostics.sort();
    let mut claimed = BTreeSet::new();
    for diagnostic in &diagnostics {
        if diagnostic.code != "unsafe_code" {
            bail!(
                "compiler emitted forbidden safety diagnostic {} at {}:{}:{}: {}",
                diagnostic.code,
                diagnostic.path,
                diagnostic.line,
                diagnostic.column,
                diagnostic.message
            );
        }
        let mut candidates: Vec<_> = sites
            .iter()
            .enumerate()
            .filter(|(_, site)| site.kind != SiteKind::LintException && site.path == diagnostic.path && site.source_range.contains(diagnostic.line, diagnostic.column))
            .collect();
        candidates.sort_by_key(|(_, site)| site.source_range.width());
        let Some((index, site)) = candidates.first() else {
            bail!(
                "compiler-expanded unsafe is absent from the lexical inventory: {}:{}:{}: {}",
                diagnostic.path,
                diagnostic.line,
                diagnostic.column,
                diagnostic.message
            );
        };
        if candidates.get(1).is_some_and(|(_, candidate)| candidate.source_range.width() == site.source_range.width()) {
            bail!("compiler-expanded unsafe maps ambiguously at {}:{}:{}", diagnostic.path, diagnostic.line, diagnostic.column);
        }
        if !claimed.insert(*index) {
            bail!(
                "multiple compiler-expanded unsafe operations map to one lexical site at {}:{}:{}",
                diagnostic.path,
                diagnostic.line,
                diagnostic.column
            );
        }
    }
    Ok(())
}

fn compare_target_diagnostics(sites: &[UnsafeSite], diagnostics: &[Diagnostic]) -> Result<()> {
    let mut by_target: std::collections::BTreeMap<&str, Vec<Diagnostic>> = std::collections::BTreeMap::new();
    for diagnostic in diagnostics {
        by_target.entry(&diagnostic.target).or_default().push(diagnostic.clone());
    }
    for diagnostics in by_target.values() {
        compare_diagnostics(sites, diagnostics)?;
    }
    Ok(())
}

fn subtract_diagnostics(observed: &[Diagnostic], baseline: &[Diagnostic]) -> Vec<Diagnostic> {
    let mut remaining: std::collections::BTreeMap<&Diagnostic, usize> = std::collections::BTreeMap::new();
    for diagnostic in baseline {
        *remaining.entry(diagnostic).or_default() += 1;
    }
    observed
        .iter()
        .filter_map(|diagnostic| {
            let count = remaining.entry(diagnostic).or_default();
            if *count == 0 {
                Some(diagnostic.clone())
            } else {
                *count -= 1;
                None
            }
        })
        .collect()
}

mod dep_info;
#[cfg(test)]
mod tests;
