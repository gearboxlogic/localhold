use std::path::Path;

use anyhow::Result;

use self::classify::Inventory;
use self::concrete_store_policy::ConcreteStorePolicy;
use self::import_policy::ImportPolicy;
use self::manifest::StructureManifest;

mod classify;
mod concrete_store_policy;
mod import_policy;
mod manifest;
mod syntax;

const CONCRETE_STORE_POLICY_PATH: &str = "policy/maintainability/concrete-stores.json";
const IMPORT_POLICY_PATH: &str = "policy/maintainability/architecture.json";
const MANIFEST_PATH: &str = "policy/maintainability/structure.json";
const TRACKED_ROOTS: [&str; 3] = ["src", "tests", "benches"];

pub fn check(workspace: &Path) -> Result<()> {
    let manifest = StructureManifest::load(&workspace.join(MANIFEST_PATH))?;
    let import_policy = ImportPolicy::load(&workspace.join(IMPORT_POLICY_PATH))?;
    let concrete_store_policy = ConcreteStorePolicy::load(&workspace.join(CONCRETE_STORE_POLICY_PATH))?;
    import_policy.require_baseline_commit(&manifest.baseline_commit)?;
    concrete_store_policy.require_baseline_commit(&manifest.baseline_commit)?;
    let current = classify::scan_workspace(workspace, &manifest.tracked_roots)?;
    manifest.compare_current(&current)?;
    import_policy.compare_current(&current)?;
    concrete_store_policy.compare_current(&current, &manifest.current_component_paths()?)?;
    let baseline = classify::scan_revision(workspace, &manifest.baseline_commit, &manifest.tracked_roots)?;
    manifest.compare_baseline(&baseline)?;
    import_policy.compare_baseline(&baseline)?;
    let current_component_paths = manifest.current_component_paths()?;
    let baseline_component_paths = manifest.baseline_component_paths()?;
    concrete_store_policy.compare_baseline(&baseline, &baseline_component_paths)?;
    concrete_store_policy.compare_site_fingerprints(&current, &baseline, &current_component_paths, &baseline_component_paths)?;
    manifest.compare_previous_revision(workspace, &current)?;
    import_policy.compare_previous_revision(workspace)?;
    concrete_store_policy.compare_previous_revision(workspace)
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
