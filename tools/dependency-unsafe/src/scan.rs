use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::cargo_env::{CargoEnvironment, temporary_root};
use crate::cargo_graph::DependencyPackage;

#[derive(Clone, Debug)]
pub struct SourceAssessment {
    pub rust_unsafe_present: bool,
    pub signals: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
struct CargoChecksum {
    package: String,
    files: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct CargoManifest {
    package: CargoPackage,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
    links: Option<String>,
}

pub fn vendor(workspace: &Path, cargo: &CargoEnvironment) -> Result<TempDir> {
    let root = temporary_root(workspace)?;
    let directory = tempfile::Builder::new().prefix("vendor-").tempdir_in(root).context("create temporary vendor directory")?;
    let output = cargo
        .cargo_command()?
        .args(["vendor", "--frozen", "--quiet", "--versioned-dirs"])
        .args(["--manifest-path".as_ref(), workspace.join("Cargo.toml").as_os_str()])
        .arg(directory.path())
        .output()
        .context("run cargo vendor")?;
    if !output.status.success() {
        bail!("cargo vendor exited {}: {}", output.status, String::from_utf8_lossy(&output.stderr));
    }
    Ok(directory)
}

pub fn assess(vendor_root: &Path, package: &DependencyPackage) -> Result<SourceAssessment> {
    let package_root = vendor_root.join(format!("{}-{}", package.name, package.version));
    if !package_root.is_dir() {
        bail!("vendored source is missing for {} {} at {}", package.name, package.version, package_root.display());
    }
    verify_package_checksum(&package_root, &package.checksum)?;
    scan_package(&package_root, package).with_context(|| format!("scan {} {}", package.name, package.version))
}

fn verify_package_checksum(package_root: &Path, expected_package: &str) -> Result<()> {
    let checksum_path = package_root.join(".cargo-checksum.json");
    let bytes = fs::read(&checksum_path).with_context(|| format!("read {}", checksum_path.display()))?;
    let checksum: CargoChecksum = serde_json::from_slice(&bytes).with_context(|| format!("parse {}", checksum_path.display()))?;
    if checksum.package != expected_package {
        bail!(
            "package checksum mismatch at {}: expected {}, found {}",
            package_root.display(),
            expected_package,
            checksum.package
        );
    }

    let files = regular_files(package_root)?;
    let expected_paths: BTreeSet<_> = checksum.files.keys().map(PathBuf::from).collect();
    let observed_paths: BTreeSet<_> = files
        .iter()
        .map(|path| path.strip_prefix(package_root).map(Path::to_path_buf).context("vendored path escaped its package root"))
        .collect::<Result<_>>()?;
    let checksum_path = PathBuf::from(".cargo-checksum.json");
    let observed_without_manifest: BTreeSet<_> = observed_paths.into_iter().filter(|path| path != &checksum_path).collect();
    if expected_paths != observed_without_manifest {
        let missing: Vec<_> = expected_paths.difference(&observed_without_manifest).collect();
        let extra: Vec<_> = observed_without_manifest.difference(&expected_paths).collect();
        bail!("vendored file set mismatch at {}: missing={missing:?}, extra={extra:?}", package_root.display());
    }
    for (relative, expected) in checksum.files {
        let actual = sha256_file(&package_root.join(&relative))?;
        if actual != expected {
            bail!("vendored file checksum mismatch for {relative}: expected {expected}, found {actual}");
        }
    }
    Ok(())
}

fn scan_package(package_root: &Path, expected: &DependencyPackage) -> Result<SourceAssessment> {
    let mut signals = BTreeSet::new();
    for path in regular_files(package_root)? {
        let extension = path.extension().and_then(OsStr::to_str).map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("rs") => scan_rust_file(&path, &mut signals)?,
            Some("c" | "cc" | "cpp" | "cxx" | "m" | "mm" | "h" | "hh" | "hpp" | "hxx" | "s" | "asm" | "cu" | "cuh" | "metal") => {
                signals.insert("native-source".to_owned());
            }
            _ if is_prebuilt_native(&path, extension.as_deref()) => {
                signals.insert("prebuilt-native".to_owned());
            }
            _ => {}
        }
    }
    let cargo_toml = fs::read_to_string(package_root.join("Cargo.toml")).with_context(|| format!("read Cargo.toml in {}", package_root.display()))?;
    let manifest: CargoManifest = toml::from_str(&cargo_toml).with_context(|| format!("parse Cargo.toml in {}", package_root.display()))?;
    if manifest.package.name != expected.name || manifest.package.version != expected.version {
        bail!(
            "vendored manifest identity mismatch: expected {} {}, found {} {}",
            expected.name,
            expected.version,
            manifest.package.name,
            manifest.package.version
        );
    }
    if manifest.package.links.is_some() {
        signals.insert("native-link".to_owned());
    }
    let rust_unsafe_present = signals.contains("rust-unsafe-syntax");
    Ok(SourceAssessment { rust_unsafe_present, signals })
}

fn is_prebuilt_native(path: &Path, extension: Option<&str>) -> bool {
    if matches!(
        extension,
        Some("a" | "o" | "obj" | "so" | "dylib" | "dll" | "lib" | "rlib" | "wasm" | "bc" | "exe" | "node" | "def" | "pdb" | "ptx" | "cubin" | "fatbin" | "spv")
    ) {
        return true;
    }
    path.file_name()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .and_then(|name| name.rsplit_once(".so.").map(|(_, version)| version.to_owned()))
        .is_some_and(|version| !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit() || byte == b'.'))
}

fn scan_rust_file(path: &Path, signals: &mut BTreeSet<String>) -> Result<()> {
    let source = fs::read_to_string(path).with_context(|| format!("read Rust source {}", path.display()))?;
    let tokens: TokenStream = rust_body(&source)?
        .parse()
        .map_err(|error| anyhow::anyhow!("parse Rust token trees {}: {error}", path.display()))?;
    scan_tokens(tokens, signals);
    Ok(())
}

fn rust_body(source: &str) -> Result<&str> {
    let without_shebang = source
        .strip_prefix("#!")
        .filter(|_| !source.starts_with("#!["))
        .and_then(|_| source.split_once('\n').map(|(_, body)| body))
        .unwrap_or(source);
    if let Some(frontmatter) = without_shebang.strip_prefix("---cargo\n") {
        return frontmatter.split_once("\n---\n").map(|(_, body)| body).context("unterminated Cargo script frontmatter");
    }
    if let Some(frontmatter) = without_shebang.strip_prefix("---cargo\r\n") {
        return frontmatter.split_once("\r\n---\r\n").map(|(_, body)| body).context("unterminated Cargo script frontmatter");
    }
    Ok(without_shebang)
}

fn scan_tokens(tokens: TokenStream, signals: &mut BTreeSet<String>) {
    let tokens: Vec<_> = tokens.into_iter().collect();
    for (index, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Group(group) => {
                if group.delimiter() == Delimiter::Bracket && previous_is_attribute_prefix(&tokens, index) && attribute_contains_link(group.stream()) {
                    signals.insert("native-link".to_owned());
                }
                scan_tokens(group.stream(), signals);
            }
            TokenTree::Ident(identifier) if identifier == "unsafe" => {
                signals.insert("rust-unsafe-syntax".to_owned());
            }
            TokenTree::Ident(identifier) if matches!(identifier.to_string().as_str(), "asm" | "global_asm" | "llvm_asm" | "naked_asm") && next_is_bang(&tokens, index) => {
                signals.insert("assembly".to_owned());
            }
            TokenTree::Ident(identifier) if identifier == "include" && tokens[index + 1..].iter().take(3).any(token_mentions_out_dir) => {
                signals.insert("generated-rust".to_owned());
            }
            TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => {}
        }
    }
}

fn attribute_contains_link(tokens: TokenStream) -> bool {
    let tokens: Vec<_> = tokens.into_iter().collect();
    tokens.iter().enumerate().any(|(index, token)| match token {
        TokenTree::Ident(identifier) if identifier == "link" => tokens
            .get(index + 1)
            .is_some_and(|next| matches!(next, TokenTree::Group(group) if group.delimiter() == Delimiter::Parenthesis)),
        TokenTree::Group(group) => attribute_contains_link(group.stream()),
        TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

fn next_is_bang(tokens: &[TokenTree], index: usize) -> bool {
    tokens
        .get(index + 1)
        .is_some_and(|token| matches!(token, TokenTree::Punct(punctuation) if punctuation.as_char() == '!'))
}

fn previous_is_attribute_prefix(tokens: &[TokenTree], index: usize) -> bool {
    let Some(previous) = index.checked_sub(1) else {
        return false;
    };
    if matches!(tokens.get(previous), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '#') {
        return true;
    }
    matches!(tokens.get(previous), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == '!')
        && previous
            .checked_sub(1)
            .and_then(|hash| tokens.get(hash))
            .is_some_and(|token| matches!(token, TokenTree::Punct(punctuation) if punctuation.as_char() == '#'))
}

fn token_mentions_out_dir(token: &TokenTree) -> bool {
    match token {
        TokenTree::Group(group) => group.stream().to_string().contains("OUT_DIR"),
        TokenTree::Literal(literal) => literal.to_string().contains("OUT_DIR"),
        TokenTree::Ident(identifier) => identifier == "OUT_DIR",
        TokenTree::Punct(_) => false,
    }
}

fn regular_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut entries: Vec<_> = fs::read_dir(&directory)
            .with_context(|| format!("read directory {}", directory.display()))?
            .collect::<std::io::Result<_>>()
            .with_context(|| format!("read entry in {}", directory.display()))?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let file_type = entry.file_type().with_context(|| format!("inspect {}", entry.path().display()))?;
            if file_type.is_symlink() {
                bail!("vendored source contains symlink {}", entry.path().display());
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
            } else {
                bail!("vendored source contains unsupported entry {}", entry.path().display());
            }
        }
    }
    files.sort();
    Ok(files)
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
#[path = "tests/scan.rs"]
mod tests;
