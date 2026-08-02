mod artifact;
mod cargo_env;
mod cargo_graph;
mod config;
mod report;
mod scan;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::cargo_env::CargoEnvironment;
use crate::config::{AuditConfig, ClassificationPolicy};

const TOOL_VERSION: &str = "1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Command {
    Check,
    Generate,
    Inventory,
}

#[derive(Debug)]
struct Args {
    command: Command,
    platform: String,
    output: Option<PathBuf>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("dependency unsafe audit failed: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = parse_args(env::args().skip(1))?;
    let workspace = workspace_root()?;
    let matrix_path = workspace.join("policy/dependency-unsafe/matrix.json");
    let classifications_path = workspace.join("policy/dependency-unsafe/classifications");
    let matrix = AuditConfig::load(&matrix_path)?;
    let classifications = ClassificationPolicy::load(&classifications_path)?;
    let platform = matrix.platform(&args.platform)?;
    if matches!(args.command, Command::Check | Command::Generate) && args.platform != current_platform()? {
        bail!(
            "check and generate require the native {} baseline; cross-target inventory is diagnostic only",
            current_platform()?
        );
    }
    if matches!(args.command, Command::Check | Command::Generate) {
        cargo_graph::require_native_target(&platform.target)?;
    }

    let cargo = CargoEnvironment::prepare(&workspace)?;
    cargo_graph::verify_toolchain(&cargo, &matrix)?;
    let generated = report::generate(&report::GenerateRequest {
        workspace: &workspace,
        cargo: &cargo,
        classifications_path: &classifications_path,
        matrix: &matrix,
        classifications: &classifications,
        platform,
        require_classifications: args.command != Command::Inventory,
        tool_version: TOOL_VERSION,
    })?;

    match args.command {
        Command::Check => {
            let baseline_path = workspace.join(&platform.baseline);
            let actual_path = workspace.join("target/dependency-unsafe").join(format!("actual-{}", platform.name));
            generated.check(&workspace, &baseline_path, &actual_path).with_context(|| {
                if platform.name == "windows" {
                    "inspect the dependency change; Windows CI uploads the complete actual-windows evidence, \
                     or regenerate with `just dependency-unsafe-generate windows` on a native Windows checkout"
                        .to_owned()
                } else {
                    "inspect the dependency change and run `just dependency-unsafe-generate linux` after approval".to_owned()
                }
            })?;
            if !report::validate_classification_coverage(&workspace, &matrix, &classifications)? {
                eprintln!("classification union validation is deferred until every native baseline is present");
            }
            println!("dependency unsafe audit passed for {} ({})", platform.name, platform.target);
        }
        Command::Generate => {
            let output = workspace.join(&platform.baseline);
            generated.write(&workspace, &output)?;
            println!("wrote {}", output.display());
        }
        Command::Inventory => {
            let output = inventory_output(&workspace, args.output.as_deref().context("inventory requires --output PATH")?)?;
            generated.write(&workspace, &output)?;
            println!("wrote {}", output.display());
        }
    }
    Ok(())
}

fn parse_args(arguments: impl Iterator<Item = String>) -> Result<Args> {
    let mut arguments = arguments;
    let command = match arguments.next().as_deref() {
        Some("check") => Command::Check,
        Some("generate") => Command::Generate,
        Some("inventory") => Command::Inventory,
        _ => bail!(
            "usage: localhold-dependency-unsafe \
             <check|generate|inventory> [--platform <linux|windows>] [--output PATH]"
        ),
    };
    let mut platform = current_platform()?.to_owned();
    let mut output = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--platform" => {
                platform = arguments.next().context("--platform requires a value")?;
            }
            "--output" => {
                output = Some(PathBuf::from(arguments.next().context("--output requires a value")?));
            }
            unknown => bail!("unknown argument {unknown:?}"),
        }
    }
    if command != Command::Inventory && output.is_some() {
        bail!("only inventory accepts --output");
    }
    if command == Command::Inventory && output.is_none() {
        bail!("inventory requires --output");
    }
    Ok(Args { command, platform, output })
}

fn inventory_output(workspace: &Path, output: &Path) -> Result<PathBuf> {
    let expected_parent = Path::new("target/dependency-unsafe");
    let name = output.file_name().and_then(|name| name.to_str()).context("inventory output name is not UTF-8")?;
    if output.is_absolute() || output.parent() != Some(expected_parent) || !name.starts_with("inventory-") || name == "inventory-" {
        bail!("inventory output must be target/dependency-unsafe/inventory-<name>");
    }
    Ok(workspace.join(output))
}

fn current_platform() -> Result<&'static str> {
    match env::consts::OS {
        "linux" => Ok("linux"),
        "windows" => Ok("windows"),
        other => bail!("dependency unsafe audit is not configured for host OS {other:?}"),
    }
}

fn workspace_root() -> Result<PathBuf> {
    if let Some(workspace) = env::var_os("LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT").filter(|value| !value.is_empty()) {
        let workspace = PathBuf::from(workspace);
        if !workspace.is_absolute() {
            bail!("dependency unsafe audit root must be absolute");
        }
        return fs::canonicalize(&workspace).with_context(|| format!("resolve audit workspace {}", workspace.display()));
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("audit tool must remain under tools/dependency-unsafe")?;
    fs::canonicalize(&workspace).with_context(|| format!("resolve audit workspace {}", workspace.display()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Command, inventory_output, parse_args};

    #[test]
    fn parser_rejects_output_for_check() {
        let error = parse_args(["check", "--output", "report.json"].into_iter().map(str::to_owned)).expect_err("check output must be fixed by policy");
        assert!(error.to_string().contains("only inventory"));
    }

    #[test]
    fn parser_accepts_explicit_generate_platform() {
        let args = parse_args(["generate", "--platform", "windows"].into_iter().map(str::to_owned)).expect("valid arguments");
        assert_eq!(args.command, Command::Generate);
        assert_eq!(args.platform, "windows");
    }

    #[test]
    fn parser_reserves_output_for_inventory() {
        assert!(parse_args(["generate", "--output", "target/dependency-unsafe/inventory-x"].into_iter().map(str::to_owned)).is_err());
        assert!(parse_args(std::iter::once("inventory").map(str::to_owned)).is_err());
    }

    #[test]
    fn inventory_output_is_confined_to_owned_target_directory() {
        let workspace = Path::new("/workspace");
        assert_eq!(
            inventory_output(workspace, Path::new("target/dependency-unsafe/inventory-windows")).expect("safe output"),
            Path::new("/workspace/target/dependency-unsafe/inventory-windows")
        );
        for unsafe_path in [
            "/tmp/inventory",
            "policy/dependency-unsafe/classifications",
            "target/dependency-unsafe",
            "target/dependency-unsafe/../baseline",
        ] {
            assert!(inventory_output(workspace, Path::new(unsafe_path)).is_err());
        }
    }
}
