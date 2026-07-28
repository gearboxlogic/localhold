use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use self::source::SourceScanner;
use super::classify::Inventory;

mod source;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceCategory {
    Production,
    Test,
    Benchmark,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceSuppression {
    pub path: String,
    pub component: String,
    pub item: String,
    pub scope: String,
    pub category: SourceCategory,
    pub level: String,
    pub lint: String,
    pub reason: String,
    pub macro_carried: bool,
    pub occurrence: usize,
    pub fingerprint: String,
}

pub fn scan_workspace(workspace: &Path, inventory: &Inventory, component_paths: &BTreeMap<&str, &str>) -> Result<Vec<SourceSuppression>> {
    let measurements = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    let mut sites = Vec::new();
    for (&path, &component) in component_paths {
        let measurement = measurements
            .get(path)
            .with_context(|| format!("suppression inventory path {path:?} has no structural measurement"))?;
        let category = if path.starts_with("benches/") {
            SourceCategory::Benchmark
        } else if path.starts_with("tests/") || measurement.physical_lines > 0 && measurement.production_lines == 0 {
            SourceCategory::Test
        } else {
            SourceCategory::Production
        };
        let source_path = workspace.join(path);
        let source = fs::read_to_string(&source_path).with_context(|| format!("read suppression source {}", source_path.display()))?;
        let syntax = syn::parse_file(&source).with_context(|| format!("parse suppression source {}", source_path.display()))?;
        sites.extend(SourceScanner::scan(path, component, category, &syntax)?);
    }
    sites.sort();
    Ok(sites)
}
