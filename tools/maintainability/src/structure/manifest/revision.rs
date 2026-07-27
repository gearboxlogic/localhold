use std::env;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::model::StructureManifest;
use super::validate::validate_revision;
use crate::structure::classify::{self, Inventory};

const BASE_REVISION_ENV: &str = "LOCALHOLD_MAINTAINABILITY_BASE_REV";
const POLICY_PATH: &str = "policy/maintainability/structure.json";

impl StructureManifest {
    pub fn compare_previous_revision(&self, workspace: &Path, current_inventory: &Inventory) -> Result<()> {
        self.compare_previous_revision_from(workspace, env::var(BASE_REVISION_ENV).ok().as_deref(), current_inventory)
    }

    pub(super) fn compare_previous_revision_from(&self, workspace: &Path, revision: Option<&str>, current_inventory: &Inventory) -> Result<()> {
        let Some(revision) = revision else {
            return Ok(());
        };
        if revision.is_empty() || revision.len() == 40 && revision.bytes().all(|byte| byte == b'0') {
            return Ok(());
        }
        validate_revision(revision).context("validate maintainability base revision")?;
        let object = format!("{revision}:{POLICY_PATH}");
        let output = Command::new("git")
            .current_dir(workspace)
            .args(["show", "--no-ext-diff", &object])
            .output()
            .context("read structure policy from maintainability base revision")?;
        if !output.status.success() {
            return verify_initial_policy_revision(workspace, revision, &object);
        }

        let previous: Self = serde_json::from_slice(&output.stdout).context("parse structure policy from maintainability base revision")?;
        previous.validate_previous().context("validate structure policy from maintainability base revision")?;
        let previous_inventory = classify::scan_revision(workspace, revision, &previous.tracked_roots)?;
        previous
            .compare_current(&previous_inventory)
            .context("verify structure policy evidence from maintainability base revision")?;
        self.compare_policy(&previous, &previous_inventory, current_inventory)
    }
}

fn verify_initial_policy_revision(workspace: &Path, revision: &str, object: &str) -> Result<()> {
    let status = Command::new("git")
        .current_dir(workspace)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("verify maintainability base revision")?;
    if !status.success() {
        bail!("maintainability base revision {revision} is not available");
    }
    let status = Command::new("git")
        .current_dir(workspace)
        .args(["cat-file", "-e", object])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("inspect structure policy in maintainability base revision")?;
    if status.success() {
        bail!("structure policy exists in base revision but could not be read");
    }
    Ok(())
}
