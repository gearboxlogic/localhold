mod check;
mod expanded;
mod manifest;
mod production_clippy;
mod scan;
mod structure;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::manifest::UnsafeManifest;

#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    Check,
    Inventory,
    ProductionClippy,
    StructureInventory { revision: Option<String> },
    SuppressionInventory,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("maintainability check failed: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let command = parse_args(env::args().skip(1))?;
    expanded::validate_authenticated_compiler_environment()?;
    let workspace = workspace_root()?;
    match command {
        Command::Check => {
            let manifest_path = workspace.join("policy/maintainability/unsafe.json");
            let manifest = UnsafeManifest::load(&manifest_path, &workspace)?;
            check::run(&workspace, &manifest)?;
            structure::check(&workspace)?;
            println!("first-party unsafe safety contract check passed");
            println!("source structure budget check passed");
            println!("lint suppression governance check passed");
        }
        Command::Inventory => {
            let roots = UnsafeManifest::required_roots();
            let sites = scan::scan_workspace(&workspace, &roots)?;
            println!("{}", serde_json::to_string_pretty(&sites)?);
        }
        Command::ProductionClippy => production_clippy::run(&workspace)?,
        Command::SuppressionInventory => {
            let inventory = structure::suppression_inventory(&workspace)?;
            println!("{}", serde_json::to_string_pretty(&inventory)?);
        }
        Command::StructureInventory { revision } => {
            let inventory = if let Some(revision) = revision {
                structure::scan_revision(&workspace, &revision)?
            } else {
                structure::scan_workspace(&workspace)?
            };
            println!("{}", serde_json::to_string_pretty(&inventory)?);
        }
    }
    Ok(())
}

fn parse_args(arguments: impl Iterator<Item = String>) -> Result<Command> {
    let mut arguments = arguments;
    let command = match arguments.next().as_deref() {
        Some("check") => Command::Check,
        Some("inventory") => Command::Inventory,
        Some("production-clippy") => Command::ProductionClippy,
        Some("structure-inventory") => Command::StructureInventory { revision: arguments.next() },
        Some("suppression-inventory") => Command::SuppressionInventory,
        _ => bail!("usage: localhold-maintainability <check|inventory|production-clippy|structure-inventory [REVISION]|suppression-inventory>"),
    };
    if let Some(argument) = arguments.next() {
        bail!("unexpected argument {argument:?}");
    }
    Ok(command)
}

fn workspace_root() -> Result<PathBuf> {
    if let Some(workspace) = env::var_os("LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT").filter(|value| !value.is_empty()) {
        let workspace = PathBuf::from(workspace);
        if !workspace.is_absolute() {
            bail!("maintainability audit root must be absolute");
        }
        return fs::canonicalize(&workspace).with_context(|| format!("resolve audit workspace {}", workspace.display()));
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("maintainability tool must remain under tools/maintainability")?;
    fs::canonicalize(&workspace).with_context(|| format!("resolve workspace {}", workspace.display()))
}

#[cfg(test)]
mod tests {
    use super::{Command, parse_args};

    #[test]
    fn parser_accepts_only_closed_command_set() {
        assert_eq!(parse_args(std::iter::once("check".to_owned())).expect("check command"), Command::Check);
        assert_eq!(parse_args(std::iter::once("inventory".to_owned())).expect("inventory command"), Command::Inventory);
        assert_eq!(
            parse_args(std::iter::once("production-clippy".to_owned())).expect("production Clippy command"),
            Command::ProductionClippy
        );
        assert_eq!(
            parse_args(std::iter::once("suppression-inventory".to_owned())).expect("suppression inventory command"),
            Command::SuppressionInventory
        );
        assert_eq!(
            parse_args(std::iter::once("structure-inventory".to_owned())).expect("structure inventory command"),
            Command::StructureInventory { revision: None }
        );
        assert_eq!(
            parse_args(["structure-inventory", "a-revision"].into_iter().map(str::to_owned)).expect("revision inventory command"),
            Command::StructureInventory {
                revision: Some("a-revision".to_owned())
            }
        );
        assert!(parse_args(std::iter::empty()).is_err());
        assert!(parse_args(["check", "extra"].into_iter().map(str::to_owned)).is_err());
    }
}
