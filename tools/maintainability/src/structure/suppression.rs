use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use self::source::SourceScanner;
use super::classify::Inventory;

mod policy;
mod source;
pub(super) use policy::SuppressionPolicy;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceCategory {
    Production,
    Test,
    Benchmark,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceSuppression {
    pub id: String,
    pub path: String,
    pub component: String,
    pub item: String,
    pub scope: String,
    pub signature: Option<String>,
    pub target: Option<String>,
    pub category: SourceCategory,
    pub level: String,
    pub lint: String,
    pub reason: String,
    pub macro_carried: bool,
    pub occurrence: usize,
    pub fingerprint: String,
}

impl SourceSuppression {
    fn stable_id(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"localhold-lint-suppression-v2");
        for field in [
            self.component.as_str(),
            self.item.as_str(),
            self.scope.as_str(),
            self.signature.as_deref().unwrap_or("<no-signature>"),
            self.target.as_deref().unwrap_or("<no-target>"),
            self.category.as_str(),
            self.level.as_str(),
            self.lint.as_str(),
            self.reason.as_str(),
            self.fingerprint.as_str(),
        ] {
            digest.update(field.len().to_string().as_bytes());
            digest.update(b":");
            digest.update(field.as_bytes());
        }
        digest.update([u8::from(self.macro_carried)]);
        format!("source.{:x}", digest.finalize())
    }
}

impl SourceCategory {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Test => "test",
            Self::Benchmark => "benchmark",
        }
    }
}

pub fn scan_workspace(workspace: &Path, inventory: &Inventory, component_paths: &BTreeMap<&str, &str>) -> Result<Vec<SourceSuppression>> {
    scan_with(inventory, component_paths, |path| {
        let source_path = workspace.join(path);
        fs::read_to_string(&source_path).with_context(|| format!("read suppression source {}", source_path.display()))
    })
}

pub fn scan_revision(workspace: &Path, revision: &str, inventory: &Inventory, component_paths: &BTreeMap<&str, &str>) -> Result<Vec<SourceSuppression>> {
    scan_with(inventory, component_paths, |path| {
        let object = format!("{revision}:{path}");
        let output = Command::new("git")
            .current_dir(workspace)
            .args(["show", "--no-ext-diff", &object])
            .output()
            .with_context(|| format!("read suppression source {path:?} from revision"))?;
        if !output.status.success() {
            anyhow::bail!("cannot read suppression source {path:?} from revision {revision}");
        }
        String::from_utf8(output.stdout).with_context(|| format!("suppression source {path:?} from revision is not UTF-8"))
    })
}

fn scan_with(inventory: &Inventory, component_paths: &BTreeMap<&str, &str>, mut read_source: impl FnMut(&str) -> Result<String>) -> Result<Vec<SourceSuppression>> {
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
        let source = read_source(path)?;
        let syntax = syn::parse_file(&source).with_context(|| format!("parse suppression source {path}"))?;
        sites.extend(SourceScanner::scan(path, component, category, &syntax)?);
    }
    sites.sort();
    Ok(sites)
}
