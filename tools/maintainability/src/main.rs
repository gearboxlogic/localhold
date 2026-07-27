mod check;
mod expanded;
mod manifest;
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
    StructureInventory { revision: Option<String> },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("maintainability check failed: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let command = parse_args(env::args().skip(1))?;
    let workspace = workspace_root()?;
    match command {
        Command::Check => {
            let manifest_path = workspace.join("policy/maintainability/unsafe.json");
            let manifest = UnsafeManifest::load(&manifest_path, &workspace)?;
            check::run(&workspace, &manifest)?;
            structure::check(&workspace)?;
            println!("first-party unsafe safety contract check passed");
            println!("source structure budget check passed");
        }
        Command::Inventory => {
            let roots = UnsafeManifest::required_roots();
            let sites = scan::scan_workspace(&workspace, &roots)?;
            println!("{}", serde_json::to_string_pretty(&sites)?);
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
        Some("structure-inventory") => Command::StructureInventory { revision: arguments.next() },
        _ => bail!("usage: localhold-maintainability <check|inventory|structure-inventory [REVISION]>"),
    };
    if let Some(argument) = arguments.next() {
        bail!("unexpected argument {argument:?}");
    }
    Ok(command)
}

fn workspace_root() -> Result<PathBuf> {
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
