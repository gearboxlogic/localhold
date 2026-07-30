use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

pub(super) fn script_commands(path: &str, source: &str) -> Option<Result<Vec<String>>> {
    is_package_json(path).then(|| parse_script_commands(source))
}

fn parse_script_commands(source: &str) -> Result<Vec<String>> {
    let package: Value = serde_json::from_str(source).context("parse package.json command surface")?;
    let package = package.as_object().ok_or_else(|| anyhow::anyhow!("package.json command surface must be an object"))?;
    let Some(scripts) = package.get("scripts") else {
        return Ok(Vec::new());
    };
    let Some(scripts) = scripts.as_object() else {
        bail!("package.json scripts must be an object");
    };
    scripts
        .iter()
        .map(|(name, command)| {
            command
                .as_str()
                .map(ToOwned::to_owned)
                .with_context(|| format!("package.json script {name:?} must be a string"))
        })
        .collect()
}

fn is_package_json(path: &str) -> bool {
    Path::new(path).file_name().and_then(|name| name.to_str()) == Some("package.json")
}
