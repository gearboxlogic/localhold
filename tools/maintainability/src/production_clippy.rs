use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::expanded::{cargo_clippy_command, sanitize_compiler_environment};

const REQUIRED_FEATURES: [&str; 3] = ["reranker", "reranker-cuda", "testing"];
const DENIED_LINTS: [&str; 2] = ["warnings", "clippy::unwrap_used"];
const PRODUCTION_PROFILE_ARGUMENT: &str = "--release";

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    manifest_path: PathBuf,
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Target {
    Lib,
    Bins,
}

impl Target {
    const fn argument(self) -> &'static str {
        match self {
            Self::Lib => "--lib",
            Self::Bins => "--bins",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Lane {
    profile: &'static str,
    feature: Option<&'static str>,
    target: Target,
}

const LANES: [Lane; 6] = [
    Lane {
        profile: "default",
        feature: None,
        target: Target::Lib,
    },
    Lane {
        profile: "default",
        feature: None,
        target: Target::Bins,
    },
    Lane {
        profile: "reranker",
        feature: Some("reranker"),
        target: Target::Lib,
    },
    Lane {
        profile: "reranker",
        feature: Some("reranker"),
        target: Target::Bins,
    },
    Lane {
        profile: "reranker-cuda",
        feature: Some("reranker-cuda"),
        target: Target::Lib,
    },
    Lane {
        profile: "reranker-cuda",
        feature: Some("reranker-cuda"),
        target: Target::Bins,
    },
];

pub fn run(workspace: &Path) -> Result<()> {
    verify_feature_contract(workspace)?;
    for lane in LANES {
        let status = lane_command(workspace, lane)
            .status()
            .with_context(|| format!("run production Clippy profile {:?} target {}", lane.profile, lane.target.argument()))?;
        if !status.success() {
            bail!("production Clippy profile {:?} target {} failed with {status}", lane.profile, lane.target.argument());
        }
    }
    verify_sentinels(workspace)?;
    println!("production Clippy matrix passed");
    Ok(())
}

fn verify_feature_contract(workspace: &Path) -> Result<()> {
    let features = load_feature_names(workspace)?;
    verify_feature_names(&features)
}

fn load_feature_names(workspace: &Path) -> Result<BTreeSet<String>> {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(workspace)
        .args(["metadata", "--format-version=1", "--no-deps", "--locked"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    sanitize_compiler_environment(&mut command);
    let output = command.output().context("run locked Cargo metadata for production feature coverage")?;
    if !output.status.success() {
        bail!("locked Cargo metadata for production feature coverage failed with {}", output.status);
    }

    let metadata: CargoMetadata = serde_json::from_slice(&output.stdout).context("parse Cargo metadata for production feature coverage")?;
    let expected_manifest = fs::canonicalize(workspace.join("Cargo.toml")).with_context(|| format!("resolve production package manifest under {}", workspace.display()))?;
    let mut root_features = None;
    for package in metadata.packages {
        let manifest_path = fs::canonicalize(&package.manifest_path).with_context(|| format!("resolve Cargo package manifest {}", package.manifest_path.display()))?;
        if manifest_path == expected_manifest && root_features.replace(package.features).is_some() {
            bail!("Cargo metadata contains duplicate entries for the production package");
        }
    }
    let features = root_features
        .context("Cargo metadata does not contain the production package")?
        .into_keys()
        .collect::<BTreeSet<_>>();
    Ok(features)
}

fn verify_feature_names(features: &BTreeSet<String>) -> Result<()> {
    let required = REQUIRED_FEATURES.map(str::to_owned).into_iter().collect::<BTreeSet<_>>();
    if *features != required {
        bail!("production Clippy profile coverage is incomplete: expected features {required:?}, found {features:?}");
    }
    Ok(())
}

fn lane_command(workspace: &Path, lane: Lane) -> Command {
    let mut command = cargo_clippy_command();
    command
        .current_dir(workspace)
        .args([lane.target.argument(), PRODUCTION_PROFILE_ARGUMENT, "--no-default-features"]);
    if let Some(feature) = lane.feature {
        command.args(["--features", feature]);
    }
    command.args(["--locked", "--"]);
    for lint in DENIED_LINTS {
        command.args(["-D", lint]);
    }
    sanitize_compiler_environment(&mut command);
    command
}

fn verify_sentinels(workspace: &Path) -> Result<()> {
    let manifest = workspace.join("tools/maintainability/fixtures/production-clippy/Cargo.toml");
    let target = workspace.join("target/production-clippy-sentinels");

    let library = sentinel_command(workspace, &manifest, &target, &["--lib", "--features", "lib-violation"], true)?;
    require_lint_failure("library unwrap sentinel", &library)?;

    let binary = sentinel_command(workspace, &manifest, &target, &["--bin", "unwrap-violation"], true)?;
    require_lint_failure("binary unwrap sentinel", &binary)?;

    let production = sentinel_command(workspace, &manifest, &target, &["--lib"], true)?;
    require_success("test-only unwrap production exclusion sentinel", &production)?;

    let tests = sentinel_command(workspace, &manifest, &target, &["--tests"], false)?;
    require_success("test-only unwrap policy sentinel", &tests)
}

fn sentinel_command(workspace: &Path, manifest: &Path, target: &Path, selection: &[&str], deny_unwrap: bool) -> Result<Output> {
    let mut command = cargo_clippy_command();
    command
        .current_dir(workspace)
        .env("CARGO_TARGET_DIR", target)
        .args([PRODUCTION_PROFILE_ARGUMENT, "--manifest-path"])
        .arg(manifest)
        .args(selection)
        .args(["--locked", "--", "-D", "warnings"]);
    if deny_unwrap {
        command.args(["-D", "clippy::unwrap_used"]);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    sanitize_compiler_environment(&mut command);
    command.output().context("run production Clippy sentinel")
}

fn require_lint_failure(label: &str, output: &Output) -> Result<()> {
    if output.status.success() {
        bail!("{label} unexpectedly passed");
    }
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    if !diagnostics.contains("clippy::unwrap-used") {
        bail!("{label} failed without the protected unwrap_used diagnostic");
    }
    Ok(())
}

fn require_success(label: &str, output: &Output) -> Result<()> {
    if !output.status.success() {
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        bail!("{label} failed with {}: {diagnostics}", output.status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_covers_every_production_profile_and_target_without_testing() {
        let observed = LANES.iter().map(|lane| (lane.profile, lane.feature, lane.target)).collect::<BTreeSet<_>>();
        let expected = ["default", "reranker", "reranker-cuda"]
            .into_iter()
            .flat_map(|profile| {
                let feature = (profile != "default").then_some(profile);
                [Target::Lib, Target::Bins].map(move |target| (profile, feature, target))
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(observed, expected);
        assert!(LANES.iter().all(|lane| lane.feature != Some("testing")));
    }

    #[test]
    fn protected_lints_are_an_exact_closed_set() {
        assert_eq!(DENIED_LINTS, ["warnings", "clippy::unwrap_used"]);
    }

    #[test]
    fn feature_contract_rejects_missing_and_uncovered_profiles() {
        let exact = REQUIRED_FEATURES.map(str::to_owned).into_iter().collect::<BTreeSet<_>>();
        verify_feature_names(&exact).expect("closed production feature set");

        let mut missing = exact.clone();
        assert!(missing.remove("reranker"));
        assert!(verify_feature_names(&missing).unwrap_err().to_string().contains("coverage is incomplete"));

        let mut uncovered = exact;
        assert!(uncovered.insert("new-production-profile".to_owned()));
        assert!(verify_feature_names(&uncovered).unwrap_err().to_string().contains("coverage is incomplete"));
    }

    #[test]
    fn cargo_metadata_exposes_implicit_optional_dependency_features() {
        let fixture = tempfile::tempdir().expect("temporary Cargo fixture");
        fs::create_dir_all(fixture.path().join("src")).expect("root source directory");
        fs::create_dir_all(fixture.path().join("implicit-backend/src")).expect("dependency source directory");
        fs::write(
            fixture.path().join("Cargo.toml"),
            r#"
[package]
name = "feature-contract-fixture"
version = "0.0.0"
edition = "2024"

[dependencies]
implicit-backend = { path = "implicit-backend", optional = true }

[features]
explicit = []

[workspace]
"#,
        )
        .expect("root manifest");
        fs::write(fixture.path().join("src/lib.rs"), "").expect("root source");
        fs::write(
            fixture.path().join("implicit-backend/Cargo.toml"),
            r#"
[package]
name = "implicit-backend"
version = "0.0.0"
edition = "2024"
"#,
        )
        .expect("dependency manifest");
        fs::write(fixture.path().join("implicit-backend/src/lib.rs"), "").expect("dependency source");

        let mut command = Command::new(env!("CARGO"));
        command
            .current_dir(fixture.path())
            .args(["metadata", "--format-version=1", "--no-deps"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        sanitize_compiler_environment(&mut command);
        let output = command.output().expect("generate fixture lockfile");
        assert!(output.status.success(), "fixture metadata failed: {}", String::from_utf8_lossy(&output.stderr));

        let features = load_feature_names(fixture.path()).expect("load Cargo feature contract");
        assert_eq!(features, ["explicit", "implicit-backend"].map(str::to_owned).into_iter().collect());
    }
}
