use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};

use crate::expanded::sanitize_compiler_environment;

const REQUIRED_FEATURES: [&str; 3] = ["reranker", "reranker-cuda", "testing"];
const DENIED_LINTS: [&str; 2] = ["warnings", "clippy::unwrap_used"];

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
    let path = workspace.join("Cargo.toml");
    let source = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let manifest: toml::Value = toml::from_str(&source).with_context(|| format!("parse {}", path.display()))?;
    let features = manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .context("Cargo.toml must define a features table")?
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    verify_feature_names(&features)
}

fn verify_feature_names(features: &BTreeSet<&str>) -> Result<()> {
    let required = REQUIRED_FEATURES.into_iter().collect::<BTreeSet<_>>();
    if *features != required {
        bail!("production Clippy profile coverage is incomplete: expected features {required:?}, found {features:?}");
    }
    Ok(())
}

fn lane_command(workspace: &Path, lane: Lane) -> Command {
    let mut command = Command::new(env!("CARGO"));
    command.current_dir(workspace).args(["clippy", lane.target.argument(), "--no-default-features"]);
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
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(workspace)
        .env("CARGO_TARGET_DIR", target)
        .args(["clippy", "--manifest-path"])
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
        let exact = REQUIRED_FEATURES.into_iter().collect::<BTreeSet<_>>();
        verify_feature_names(&exact).expect("closed production feature set");

        let mut missing = exact.clone();
        assert!(missing.remove("reranker"));
        assert!(verify_feature_names(&missing).unwrap_err().to_string().contains("coverage is incomplete"));

        let mut uncovered = exact;
        assert!(uncovered.insert("new-production-profile"));
        assert!(verify_feature_names(&uncovered).unwrap_err().to_string().contains("coverage is incomplete"));
    }
}
