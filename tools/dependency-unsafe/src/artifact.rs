use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Component;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

type ArtifactFiles = BTreeMap<String, Vec<u8>>;

#[derive(Debug)]
pub struct GeneratedArtifacts {
    pub files: ArtifactFiles,
}

impl GeneratedArtifacts {
    pub const fn new(files: ArtifactFiles) -> Self {
        Self { files }
    }

    pub fn check(&self, workspace: &Path, baseline: &Path, actual: &Path) -> Result<()> {
        let expected = match read_artifacts(workspace, baseline) {
            Ok(expected) => expected,
            Err(error) => {
                self.write(workspace, actual)?;
                return Err(error).with_context(|| format!("baseline is unavailable; regenerated evidence is in {}", actual.display()));
            }
        };
        if expected == self.files {
            return Ok(());
        }
        self.write(workspace, actual)?;
        let missing: Vec<_> = self.files.keys().filter(|name| !expected.contains_key(*name)).cloned().collect();
        let extra: Vec<_> = expected.keys().filter(|name| !self.files.contains_key(*name)).cloned().collect();
        let changed: Vec<_> = self
            .files
            .iter()
            .filter_map(|(name, bytes)| expected.get(name).filter(|expected_bytes| *expected_bytes != bytes).map(|_| name.clone()))
            .collect();
        bail!(
            "dependency baseline {} is stale; missing={missing:?}, extra={extra:?}, \
             changed={changed:?}; regenerated evidence is in {}",
            baseline.display(),
            actual.display()
        );
    }

    pub fn write(&self, workspace: &Path, directory: &Path) -> Result<()> {
        validate_artifact_names(self.files.keys().map(String::as_str))?;
        let parent = directory.parent().context("artifact directory has no parent")?;
        create_confined_directories(workspace, parent)?;
        validate_artifact_directory(directory)?;
        let stage = tempfile::Builder::new()
            .prefix(".dependency-unsafe-stage-")
            .tempdir_in(parent)
            .with_context(|| format!("create artifact stage in {}", parent.display()))?;
        for (name, bytes) in &self.files {
            let path = stage.path().join(name);
            fs::write(&path, bytes).with_context(|| format!("write artifact {}", path.display()))?;
        }
        validate_confined_components(workspace, parent)?;
        replace_directory(&stage.keep(), directory)?;
        Ok(())
    }
}

fn validate_artifact_names<'a>(names: impl Iterator<Item = &'a str>) -> Result<()> {
    for name in names {
        let path = Path::new(name);
        if path.file_name().and_then(|value| value.to_str()) != Some(name) || !matches!(path.extension().and_then(|extension| extension.to_str()), Some("json" | "jsonl")) {
            bail!("unsupported artifact file name {name:?}");
        }
    }
    Ok(())
}

fn confined_components<'a>(workspace: &'a Path, path: &'a Path) -> Result<Vec<&'a std::ffi::OsStr>> {
    let relative = path
        .strip_prefix(workspace)
        .with_context(|| format!("artifact path {} is outside trusted workspace {}", path.display(), workspace.display()))?;
    relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => Ok(value),
            Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("artifact path contains unsupported component: {}", path.display())
            }
        })
        .collect()
}

fn validate_confined_components(workspace: &Path, path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(workspace).with_context(|| format!("inspect trusted workspace {}", workspace.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("trusted workspace is not a regular directory: {}", workspace.display());
    }
    let mut current = workspace.to_path_buf();
    for component in confined_components(workspace, path)? {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!("artifact path contains non-directory or symlink component {}", current.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                bail!("artifact path component does not exist: {}", current.display());
            }
            Err(error) => return Err(error).with_context(|| format!("inspect artifact path component {}", current.display())),
        }
    }
    Ok(())
}

fn create_confined_directories(workspace: &Path, path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(workspace).with_context(|| format!("inspect trusted workspace {}", workspace.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("trusted workspace is not a regular directory: {}", workspace.display());
    }
    let mut current = workspace.to_path_buf();
    for component in confined_components(workspace, path)? {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!("artifact path contains non-directory or symlink component {}", current.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error).with_context(|| format!("create artifact path component {}", current.display())),
                }
                let metadata = fs::symlink_metadata(&current).with_context(|| format!("inspect created artifact path component {}", current.display()))?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    bail!("created artifact path component is not a regular directory: {}", current.display());
                }
            }
            Err(error) => return Err(error).with_context(|| format!("inspect artifact path component {}", current.display())),
        }
    }
    Ok(())
}

fn validate_artifact_directory(directory: &Path) -> Result<()> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!("artifact path is not a regular directory: {}", directory.display());
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspect artifact directory {}", directory.display())),
    }
    for entry in fs::read_dir(directory).with_context(|| format!("read artifact directory {}", directory.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() || !matches!(path.extension().and_then(|extension| extension.to_str()), Some("json" | "jsonl")) {
            bail!("artifact directory contains unsupported entry {}", path.display());
        }
    }
    Ok(())
}

fn replace_directory(stage: &Path, destination: &Path) -> Result<()> {
    match fs::symlink_metadata(destination) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return fs::rename(stage, destination).with_context(|| format!("install artifact directory {}", destination.display()));
        }
        Err(error) => return Err(error).with_context(|| format!("inspect artifact directory {}", destination.display())),
        Ok(_) => {}
    }
    let parent = destination.parent().context("artifact directory has no parent")?;
    let backup = tempfile::Builder::new()
        .prefix(".dependency-unsafe-backup-")
        .tempdir_in(parent)
        .with_context(|| format!("reserve artifact backup in {}", parent.display()))?
        .keep();
    fs::remove_dir(&backup).with_context(|| format!("prepare artifact backup {}", backup.display()))?;
    if let Err(error) = fs::rename(destination, &backup) {
        let _ = fs::remove_dir_all(stage);
        return Err(error).with_context(|| format!("back up artifact directory {}", destination.display()));
    }
    if let Err(error) = fs::rename(stage, destination) {
        let restore = fs::rename(&backup, destination);
        let _ = fs::remove_dir_all(stage);
        restore.with_context(|| format!("restore artifact directory {} after install failure", destination.display()))?;
        return Err(error).with_context(|| format!("install artifact directory {}", destination.display()));
    }
    fs::remove_dir_all(&backup).with_context(|| format!("remove artifact backup {}", backup.display()))
}

pub fn read_artifacts(workspace: &Path, directory: &Path) -> Result<ArtifactFiles> {
    validate_confined_components(workspace, directory)?;
    validate_artifact_directory(directory)?;
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(directory).with_context(|| format!("read baseline directory {}", directory.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|name| name.to_str()).context("baseline file name is not UTF-8")?.to_owned();
        files.insert(name, fs::read(&path).with_context(|| format!("read baseline {}", path.display()))?);
    }
    Ok(files)
}

pub fn pretty_json(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).context("serialize JSON artifact")?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn json_lines<T: Serialize>(values: &[T]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    append_json_lines(&mut bytes, values)?;
    Ok(bytes)
}

pub fn append_json_lines<T: Serialize>(bytes: &mut Vec<u8>, values: &[T]) -> Result<()> {
    for value in values {
        bytes.extend(serde_json::to_vec(value).context("serialize JSONL record")?);
        bytes.push(b'\n');
    }
    Ok(())
}

pub fn json_line(value: &impl Serialize) -> Result<Vec<u8>> {
    json_lines(std::slice::from_ref(value))
}

#[cfg(test)]
#[path = "tests/artifact.rs"]
mod tests;
