use std::path::Path;

use anyhow::Result;

use self::classify::Inventory;
use self::manifest::StructureManifest;

mod classify;
mod manifest;
mod syntax;

const MANIFEST_PATH: &str = "policy/maintainability/structure.json";
const TRACKED_ROOTS: [&str; 3] = ["src", "tests", "benches"];

pub fn check(workspace: &Path) -> Result<()> {
    let manifest = StructureManifest::load(&workspace.join(MANIFEST_PATH))?;
    let current = classify::scan_workspace(workspace, &manifest.tracked_roots)?;
    manifest.compare_current(&current)?;
    let baseline = classify::scan_revision(workspace, &manifest.baseline_commit, &manifest.tracked_roots)?;
    manifest.compare_baseline(&baseline)?;
    manifest.compare_previous_revision(workspace, &current)
}

pub fn scan_workspace(workspace: &Path) -> Result<Inventory> {
    let roots = TRACKED_ROOTS.into_iter().map(str::to_owned).collect::<Vec<_>>();
    classify::scan_workspace(workspace, &roots)
}

pub fn scan_revision(workspace: &Path, revision: &str) -> Result<Inventory> {
    let roots = TRACKED_ROOTS.into_iter().map(str::to_owned).collect::<Vec<_>>();
    classify::scan_revision(workspace, revision, &roots)
}

#[cfg(test)]
mod tests;
