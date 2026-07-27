mod check;
mod manifest;
mod scan;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::manifest::UnsafeManifest;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Command {
    Check,
    Inventory,
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
    let manifest_path = workspace.join("policy/maintainability/unsafe.json");
    match command {
        Command::Check => {
            let manifest = UnsafeManifest::load(&manifest_path)?;
            check::run(&workspace, &manifest)?;
            println!("first-party unsafe safety contract check passed");
        }
        Command::Inventory => {
            let roots = UnsafeManifest::required_roots();
            let sites = scan::scan_workspace(&workspace, &roots)?;
            println!("{}", serde_json::to_string_pretty(&sites)?);
        }
    }
    Ok(())
}

fn parse_args(arguments: impl Iterator<Item = String>) -> Result<Command> {
    let mut arguments = arguments;
    let command = match arguments.next().as_deref() {
        Some("check") => Command::Check,
        Some("inventory") => Command::Inventory,
        _ => bail!("usage: localhold-maintainability <check|inventory>"),
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
        assert!(parse_args(std::iter::empty()).is_err());
        assert!(parse_args(["check", "extra"].into_iter().map(str::to_owned)).is_err());
    }
}
