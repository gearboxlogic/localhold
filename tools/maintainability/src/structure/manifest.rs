use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

pub use self::model::StructureManifest;

mod comparison;
mod evolution;
mod exceptions;
mod measure;
mod model;
mod revision;
mod split_allowances;
mod validate;

impl StructureManifest {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read structure manifest {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes).with_context(|| format!("parse structure manifest {}", path.display()))?;
        manifest.validate_current()?;
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests;
