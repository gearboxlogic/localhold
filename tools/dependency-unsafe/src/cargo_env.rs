use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const CRATES_IO_PROTOCOL: &str = "sparse";

#[derive(Debug, Deserialize)]
struct Lockfile {
    version: u32,
    package: Vec<LockedPackage>,
}

#[derive(Debug, Deserialize)]
struct LockedPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

struct RegistryArchive {
    path: PathBuf,
    index_name: PathBuf,
}

pub struct CargoEnvironment {
    _home: TempDir,
    _cwd: TempDir,
    home_path: PathBuf,
    cwd_path: PathBuf,
    cargo: PathBuf,
    rustc: PathBuf,
}

impl CargoEnvironment {
    pub fn prepare(workspace: &Path) -> Result<Self> {
        let lockfile_path = workspace.join("Cargo.lock");
        let source = fs::read_to_string(&lockfile_path).with_context(|| format!("read {}", lockfile_path.display()))?;
        let configured_home = cargo_home()?;
        let cargo_home = fs::canonicalize(&configured_home).with_context(|| format!("resolve Cargo home {}", configured_home.display()))?;
        Self::prepare_from(&source, &cargo_home, workspace)
    }

    fn prepare_from(lockfile_source: &str, source_home: &Path, workspace: &Path) -> Result<Self> {
        Self::prepare_from_with_cwd_root(lockfile_source, source_home, workspace, None)
    }

    fn prepare_from_with_cwd_root(lockfile_source: &str, source_home: &Path, workspace: &Path, cwd_root: Option<&Path>) -> Result<Self> {
        let lockfile: Lockfile = toml::from_str(lockfile_source).context("parse Cargo.lock for isolated registry")?;
        if lockfile.version != 4 {
            bail!("unsupported Cargo.lock format {}", lockfile.version);
        }
        let temporary_root = temporary_root(workspace)?;
        let home = tempfile::Builder::new()
            .prefix("cargo-home-")
            .tempdir_in(&temporary_root)
            .context("create isolated Cargo home")?;
        let cwd = cwd_root
            .map_or_else(
                || tempfile::Builder::new().prefix("localhold-dependency-unsafe-cwd-").tempdir(),
                |root| tempfile::Builder::new().prefix("localhold-dependency-unsafe-cwd-").tempdir_in(root),
            )
            .context("create config-free Cargo working directory")?;
        let home_path = fs::canonicalize(home.path()).context("resolve isolated Cargo home")?;
        let cwd_path = fs::canonicalize(cwd.path()).context("resolve isolated Cargo working directory")?;
        reject_cargo_config(&cwd_path, &home_path)?;
        let (cargo, rustc) = toolchain_executables()?;
        let destination_cache = home_path.join("registry/cache");
        let mut index_directories = BTreeSet::new();
        for package in lockfile.package {
            let Some(source) = package.source else {
                continue;
            };
            if source != CRATES_IO_SOURCE {
                bail!("unsupported locked dependency source {source:?} for {} {}", package.name, package.version);
            }
            validate_cache_component(&package.name, "package name")?;
            validate_cache_component(&package.version, "package version")?;
            let checksum = package
                .checksum
                .filter(|value| valid_checksum(value))
                .with_context(|| format!("registry package {} {} has no normalized Cargo.lock checksum", package.name, package.version))?;
            let archive_name = format!("{}-{}.crate", package.name, package.version);
            let RegistryArchive { path: source_archive, index_name } = find_sparse_archive(source_home, &archive_name, &checksum)?;
            let destination_directory = destination_cache.join(&index_name);
            fs::create_dir_all(&destination_directory).with_context(|| format!("create isolated registry cache {}", destination_directory.display()))?;
            let destination_archive = destination_directory.join(&archive_name);
            fs::copy(&source_archive, &destination_archive).with_context(|| format!("copy verified registry archive {}", source_archive.display()))?;
            verify_digest(&destination_archive, &checksum)?;
            index_directories.insert(index_name);
        }
        for index_name in index_directories {
            let source_index = source_home.join("registry/index").join(&index_name);
            let destination_index = home_path.join("registry/index").join(&index_name);
            copy_regular_tree(&source_index, &destination_index)?;
        }
        Ok(Self {
            _home: home,
            _cwd: cwd,
            home_path,
            cwd_path,
            cargo,
            rustc,
        })
    }

    pub fn cargo_command(&self) -> Result<Command> {
        reject_cargo_config(&self.cwd_path, &self.home_path)?;
        let mut command = Command::new(&self.cargo);
        command
            .current_dir(&self.cwd_path)
            .env("CARGO_HOME", &self.home_path)
            .env("RUSTC", &self.rustc)
            .env("CARGO_BUILD_RUSTC", &self.rustc)
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("CARGO_BUILD_RUSTFLAGS");
        for (name, _) in env::vars_os() {
            if source_configuration_variable(&name) || rust_flag_variable(&name) {
                command.env_remove(name);
            }
        }
        command.env("CARGO_REGISTRIES_CRATES_IO_PROTOCOL", CRATES_IO_PROTOCOL);
        Ok(command)
    }

    pub fn cargo_command_from_vendor(&self, vendor: &Path) -> Result<Command> {
        let vendor = fs::canonicalize(vendor).with_context(|| format!("resolve verified vendor directory {}", vendor.display()))?;
        let expected_parent = self.home_path.parent().context("isolated Cargo home has no temporary root")?;
        if vendor.parent() != Some(expected_parent) || !vendor.file_name().and_then(OsStr::to_str).is_some_and(|name| name.starts_with("vendor-")) {
            bail!("verified vendor directory escaped the dependency audit temporary root: {}", vendor.display());
        }
        let metadata = fs::symlink_metadata(&vendor).with_context(|| format!("inspect verified vendor directory {}", vendor.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("verified vendor path is not a regular directory: {}", vendor.display());
        }
        let vendor = vendor.to_str().context("verified vendor directory is not UTF-8")?;
        let directory_value = toml::Value::String(vendor.to_owned()).to_string();
        let mut command = self.cargo_command()?;
        command
            .args(["--config", "source.crates-io.replace-with=\"verified-vendor\""])
            .args(["--config", &format!("source.verified-vendor.directory={directory_value}")]);
        Ok(command)
    }

    pub fn rustc_command(&self) -> Command {
        Command::new(&self.rustc)
    }
}

pub fn pinned_rustc_command() -> Result<Command> {
    let (_, rustc) = toolchain_executables()?;
    Ok(Command::new(rustc))
}

pub fn temporary_root(workspace: &Path) -> Result<PathBuf> {
    let mut root = workspace.to_path_buf();
    for component in [".cache", "dependency-unsafe"] {
        root.push(component);
        match fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!("dependency audit temporary path is not a regular directory: {}", root.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                fs::create_dir(&root).with_context(|| format!("create dependency audit temporary path {}", root.display()))?;
                let metadata = fs::symlink_metadata(&root).with_context(|| format!("inspect dependency audit temporary path {}", root.display()))?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    bail!("created dependency audit temporary path is not a regular directory: {}", root.display());
                }
            }
            Err(error) => return Err(error).with_context(|| format!("inspect dependency audit temporary path {}", root.display())),
        }
    }
    Ok(root)
}

fn cargo_home() -> Result<PathBuf> {
    if let Some(path) = env::var_os("CARGO_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .context("CARGO_HOME, HOME, and USERPROFILE are unset")?;
    Ok(PathBuf::from(home).join(".cargo"))
}

fn toolchain_executables() -> Result<(PathBuf, PathBuf)> {
    let configured_cargo = Path::new(env!("CARGO"));
    if !configured_cargo.is_absolute() {
        bail!("Cargo did not provide an absolute executable path at build time: {}", configured_cargo.display());
    }
    let cargo = fs::canonicalize(configured_cargo).with_context(|| format!("resolve build-time Cargo executable {}", configured_cargo.display()))?;
    let rustc_name = format!("rustc{}", env::consts::EXE_SUFFIX);
    let configured_rustc = cargo.parent().context("build-time Cargo executable has no parent directory")?.join(rustc_name);
    let rustc = fs::canonicalize(&configured_rustc).with_context(|| format!("resolve matching rustc executable {}", configured_rustc.display()))?;
    for (name, path) in [("cargo", &cargo), ("rustc", &rustc)] {
        let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect pinned {name} executable {}", path.display()))?;
        if !metadata.is_file() {
            bail!("pinned {name} executable is not a regular file: {}", path.display());
        }
    }
    Ok((cargo, rustc))
}

fn reject_cargo_config(cwd: &Path, cargo_home: &Path) -> Result<()> {
    for ancestor in cwd.ancestors() {
        for name in ["config", "config.toml"] {
            reject_existing_config(&ancestor.join(".cargo").join(name))?;
        }
    }
    for name in ["config", "config.toml"] {
        reject_existing_config(&cargo_home.join(name))?;
    }
    Ok(())
}

fn reject_existing_config(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("isolated Cargo invocation refuses configuration file {}", path.display()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect Cargo configuration path {}", path.display())),
    }
}

fn source_configuration_variable(name: &OsStr) -> bool {
    name.to_str()
        .map(str::to_ascii_uppercase)
        .is_some_and(|name| name.starts_with("CARGO_SOURCE_") || name.starts_with("CARGO_REGISTRIES_CRATES_IO_"))
}

fn rust_flag_variable(name: &OsStr) -> bool {
    name.to_str().map(str::to_ascii_uppercase).is_some_and(|name| {
        matches!(name.as_str(), "RUSTFLAGS" | "CARGO_ENCODED_RUSTFLAGS" | "CARGO_BUILD_RUSTFLAGS") || name.starts_with("CARGO_TARGET_") && name.ends_with("_RUSTFLAGS")
    })
}

fn validate_cache_component(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || Path::new(value).file_name().and_then(OsStr::to_str) != Some(value) {
        bail!("locked {label} is not a safe registry cache component: {value:?}");
    }
    Ok(())
}

fn valid_checksum(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn find_sparse_archive(source_home: &Path, archive_name: &str, checksum: &str) -> Result<RegistryArchive> {
    let cache_root = source_home.join("registry/cache");
    let metadata = fs::symlink_metadata(&cache_root).with_context(|| format!("inspect Cargo registry cache {}", cache_root.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("Cargo registry cache is not a regular directory: {}", cache_root.display());
    }
    let mut entries: Vec<_> = fs::read_dir(&cache_root)
        .with_context(|| format!("read Cargo registry cache {}", cache_root.display()))?
        .collect::<std::io::Result<_>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut matches = Vec::new();
    for entry in entries {
        let file_type = entry.file_type().with_context(|| format!("inspect registry cache entry {}", entry.path().display()))?;
        if !file_type.is_dir() {
            bail!("Cargo registry cache contains unsupported entry {}", entry.path().display());
        }
        let candidate = entry.path().join(archive_name);
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                bail!("registry archive is not a regular file: {}", candidate.display());
            }
            Ok(_) => matches.push(RegistryArchive {
                path: candidate,
                index_name: PathBuf::from(entry.file_name()),
            }),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("inspect registry archive {}", candidate.display())),
        }
    }
    if matches.is_empty() {
        bail!("Cargo registry cache has no candidate for {archive_name:?}; run `cargo fetch --locked`");
    }
    for archive in &matches {
        verify_digest(&archive.path, checksum)?;
    }
    let index_root = source_home.join("registry/index");
    let mut sparse_matches = matches
        .into_iter()
        .filter_map(|archive| match sparse_index(&index_root.join(&archive.index_name)) {
            Ok(true) => Some(Ok(archive)),
            Ok(false) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    if sparse_matches.len() != 1 {
        bail!(
            "Cargo registry cache has {} sparse-protocol candidates for {archive_name:?}; \
             run `cargo fetch --locked` with the default crates.io protocol",
            sparse_matches.len()
        );
    }
    Ok(sparse_matches.remove(0))
}

fn sparse_index(index: &Path) -> Result<bool> {
    let metadata = fs::symlink_metadata(index).with_context(|| format!("inspect Cargo registry index {}", index.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("Cargo registry index is not a regular directory: {}", index.display());
    }
    let marker = index.join(".cache");
    match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!("Cargo sparse registry marker is not a regular directory: {}", marker.display());
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect Cargo sparse registry marker {}", marker.display())),
    }
}

fn verify_digest(path: &Path, expected: &str) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("read registry archive {}", path.display()))?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected {
        bail!("registry archive checksum mismatch for {}: expected {expected}, found {actual}", path.display());
    }
    Ok(())
}

fn copy_regular_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source).with_context(|| format!("inspect registry index {}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("registry index is not a regular directory: {}", source.display());
    }
    fs::create_dir_all(destination).with_context(|| format!("create isolated registry index {}", destination.display()))?;
    let mut entries: Vec<_> = fs::read_dir(source)
        .with_context(|| format!("read registry index {}", source.display()))?
        .collect::<std::io::Result<_>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry.file_type().with_context(|| format!("inspect registry index entry {}", entry.path().display()))?;
        let target = destination.join(entry.file_name());
        if file_type.is_symlink() {
            bail!("registry index contains symlink {}", entry.path().display());
        }
        if file_type.is_dir() {
            copy_regular_tree(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target).with_context(|| format!("copy registry index file {}", entry.path().display()))?;
        } else {
            bail!("registry index contains unsupported entry {}", entry.path().display());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/cargo_env.rs"]
mod tests;
