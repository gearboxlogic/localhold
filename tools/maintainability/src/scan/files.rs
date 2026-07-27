use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub(super) fn collect_required(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::symlink_metadata(root).with_context(|| format!("inspect tracked source root {}", root.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("tracked source root cannot be a symlink: {}", root.display());
    }
    if !metadata.is_dir() {
        bail!("tracked source root is not a directory: {}", root.display());
    }
    let mut entries = fs::read_dir(root)
        .with_context(|| format!("read tracked source directory {}", root.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().with_context(|| format!("inspect tracked source entry {}", path.display()))?;
        if file_type.is_symlink() {
            bail!("tracked Rust source tree cannot contain symlinks: {}", path.display());
        }
        if file_type.is_dir() {
            collect_required(&path, files)?;
        } else if file_type.is_file() && path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    Ok(())
}

pub(super) fn collect_optional(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    match fs::symlink_metadata(root) {
        Ok(_) => collect_required(root, files),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect optional source root {}", root.display())),
    }
}
