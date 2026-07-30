use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};

use super::super::{parse_nul_paths, validate_relative_path};
use super::actions::validate_local_actions;
use super::is_execution_surface;

pub(super) struct ExecutionSurfaceSet {
    pub(super) paths: Vec<String>,
    pub(super) tracked_paths: BTreeSet<String>,
}

pub(super) fn execution_surfaces(workspace: &Path) -> Result<ExecutionSurfaceSet> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .output()
        .context("list checked-in command execution surfaces")?;
    if !output.status.success() {
        bail!("git ls-files failed while listing command execution surfaces");
    }
    let paths = parse_nul_paths(&output.stdout, |_| true)?.into_iter().collect::<BTreeSet<_>>();
    let tracked_paths = tracked_paths(workspace)?;
    let executables = tracked_executables(workspace)?;
    validate_local_actions(workspace, &paths)?;
    let mut surfaces = BTreeSet::new();
    for path in paths {
        let absolute = workspace.join(&path);
        match fs::symlink_metadata(&absolute) {
            Ok(metadata) if metadata.is_dir() => continue,
            Ok(metadata) if metadata.file_type().is_symlink() => bail!("command execution surface cannot be a symlink: {path:?}"),
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("inspect possible command execution surface {}", absolute.display())),
        }
        if is_execution_surface(&path) || executables.contains(&path) || has_shebang(&absolute)? {
            surfaces.insert(path);
        }
    }
    Ok(ExecutionSurfaceSet {
        paths: surfaces.into_iter().collect(),
        tracked_paths,
    })
}

fn tracked_paths(workspace: &Path) -> Result<BTreeSet<String>> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-files", "-z", "--cached"])
        .output()
        .context("list tracked command surfaces")?;
    if !output.status.success() {
        bail!("git ls-files failed while listing tracked command surfaces");
    }
    Ok(parse_nul_paths(&output.stdout, |_| true)?.into_iter().collect())
}

fn tracked_executables(workspace: &Path) -> Result<BTreeSet<String>> {
    let output = crate::structure::revision::git_command()
        .current_dir(workspace)
        .args(["ls-files", "-z", "--stage", "--cached"])
        .output()
        .context("list tracked executable command surfaces")?;
    if !output.status.success() {
        bail!("git ls-files failed while listing executable command surfaces");
    }
    let mut paths = BTreeSet::new();
    for record in output.stdout.split(|byte| *byte == b'\0').filter(|record| !record.is_empty()) {
        let record = std::str::from_utf8(record).context("tracked executable entry is not UTF-8")?;
        let (metadata, path) = record.split_once('\t').context("tracked executable entry has no path")?;
        validate_relative_path(path, "tracked executable path")?;
        if metadata.split_ascii_whitespace().next() == Some("100755") {
            paths.insert(path.to_owned());
        }
    }
    Ok(paths)
}

fn has_shebang(path: &Path) -> Result<bool> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("inspect possible command execution surface {}", path.display())),
    };
    if !metadata.is_file() {
        return Ok(false);
    }
    let mut prefix = [0_u8; 2];
    let mut file = fs::File::open(path).with_context(|| format!("inspect possible command execution surface {}", path.display()))?;
    Ok(file
        .read(&mut prefix)
        .with_context(|| format!("read possible command execution surface {}", path.display()))?
        == prefix.len()
        && prefix == *b"#!")
}
