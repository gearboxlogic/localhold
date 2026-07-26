use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_json::json;
use tempfile::tempdir;

use super::{regular_files, rust_body, scan_package, scan_rust_file, sha256_file, verify_package_checksum};
use crate::cargo_graph::DependencyPackage;

fn package(name: &str, version: &str) -> DependencyPackage {
    DependencyPackage {
        source_id: format!("crates.io:{name}@{version}#checksum"),
        name: name.to_owned(),
        version: version.to_owned(),
        checksum: "checksum".to_owned(),
        features: Vec::new(),
        build_script: false,
        proc_macro: false,
    }
}

fn write_manifest(root: &Path, name: &str, version: &str, links: Option<&str>) {
    let links = links.map_or_else(String::new, |value| format!("links = {value:?}\n"));
    fs::write(root.join("Cargo.toml"), format!("[package]\nname = {name:?}\nversion = {version:?}\n{links}")).expect("write manifest");
}

fn write_checksum(root: &Path, package_checksum: &str) {
    let mut files = BTreeMap::new();
    for path in regular_files(root).expect("fixture files") {
        let relative = path.strip_prefix(root).expect("fixture path under root").to_string_lossy().replace('\\', "/");
        if relative != ".cargo-checksum.json" {
            files.insert(relative, sha256_file(&path).expect("fixture checksum"));
        }
    }
    fs::write(
        root.join(".cargo-checksum.json"),
        serde_json::to_vec(&json!({"package": package_checksum, "files": files})).expect("serialize checksum"),
    )
    .expect("write checksum");
}

fn scan_source(source: &[u8]) -> anyhow::Result<BTreeSet<String>> {
    let directory = tempdir().expect("temporary directory");
    let path = directory.path().join("fixture.rs");
    fs::write(&path, source).expect("write fixture");
    let mut signals = BTreeSet::new();
    scan_rust_file(&path, &mut signals)?;
    Ok(signals)
}

#[test]
fn comments_strings_and_raw_identifiers_do_not_count_as_unsafe() {
    let signals = scan_source(
        b"// unsafe { hidden(); }\n\
          const WORD: &str = \"unsafe\";\n\
          const RAW: &str = r#\"unsafe\"#;\n\
          fn r#unsafe() {}\n",
    )
    .expect("parse safe fixture");
    assert!(!signals.contains("rust-unsafe-syntax"));
}

#[test]
fn executable_and_macro_unsafe_are_detected() {
    let signals = scan_source(
        b"unsafe fn boundary() { unsafe { call(); } }\n\
          macro_rules! generated { () => { unsafe { call(); } } }\n",
    )
    .expect("parse unsafe fixture");
    assert!(signals.contains("rust-unsafe-syntax"));
}

#[test]
fn malformed_or_non_utf8_source_fails_closed() {
    assert!(scan_source(b"fn broken( {").is_err());
    assert!(scan_source(&[0xff, 0xfe]).is_err());
}

#[test]
fn cargo_script_frontmatter_supports_lf_and_crlf() {
    let lf = "#!/usr/bin/env cargo\n---cargo\n[dependencies]\n---\nfn main() {}\n";
    assert_eq!(rust_body(lf).expect("valid LF frontmatter"), "fn main() {}\n");

    let crlf = "#!/usr/bin/env cargo\r\n---cargo\r\n[dependencies]\r\n---\r\nfn main() {}\r\n";
    assert_eq!(rust_body(crlf).expect("valid CRLF frontmatter"), "fn main() {}\r\n");
}

#[test]
fn unterminated_cargo_script_frontmatter_fails_closed() {
    assert!(rust_body("---cargo\n[dependencies]\n").is_err());
    assert!(rust_body("---cargo\r\n[dependencies]\r\n").is_err());
}

#[test]
fn generated_assembly_and_link_boundaries_are_detected() {
    let signals = scan_source(
        b"include!(concat!(env!(\"OUT_DIR\"), \"/bindings.rs\"));\n\
          core::arch::asm!(\"nop\");\n\
          #[link(name = \"native\")]\n\
          unsafe extern \"C\" {}\n",
    )
    .expect("parse boundary fixture");
    assert!(signals.contains("generated-rust"));
    assert!(signals.contains("assembly"));
    assert!(signals.contains("native-link"));
    assert!(signals.contains("rust-unsafe-syntax"));
}

#[test]
fn conditional_link_attribute_is_detected() {
    let signals = scan_source(
        b"#[cfg_attr(target_env = \"msvc\", link(name = \"native\", kind = \"raw-dylib\"))]\n\
          unsafe extern \"C\" {}\n",
    )
    .expect("parse conditional link fixture");
    assert!(signals.contains("native-link"));
}

#[test]
fn inner_link_attribute_is_detected() {
    let signals = scan_source(b"#![link(name = \"native\")]\nfn main() {}\n").expect("parse inner link attribute");
    assert!(signals.contains("native-link"));
}

#[test]
fn ordinary_indexing_does_not_look_like_a_link_attribute() {
    let signals = scan_source(b"fn read(links: &[u8], index: usize) { let _ = links[index]; }\n").expect("parse indexing fixture");
    assert!(!signals.contains("native-link"));
}

#[test]
fn package_scan_separates_native_and_prebuilt_signals() {
    let directory = tempdir().expect("temporary directory");
    let root = directory.path();
    write_manifest(root, "fixture", "1.2.3", Some("fixture"));
    fs::create_dir(root.join("src")).expect("create source directory");
    fs::write(root.join("src/lib.rs"), "pub fn safe() {}\n").expect("write Rust source");
    fs::write(root.join("kernel.cu"), "__global__ void fixture() {}\n").expect("write native source");
    fs::write(root.join("kernel.ptx"), ".version 8.0\n").expect("write precompiled GPU assembly");
    fs::write(root.join("libfixture.so.1"), b"native").expect("write versioned shared object");
    fs::write(root.join("libfixture.rlib"), b"native").expect("write Rust native archive");
    fs::write(root.join("module.wasm"), b"\0asm").expect("write prebuilt artifact");

    let assessment = scan_package(root, &package("fixture", "1.2.3")).expect("scan package fixture");
    assert!(!assessment.rust_unsafe_present);
    assert_eq!(
        assessment.signals,
        BTreeSet::from(["native-link".to_owned(), "native-source".to_owned(), "prebuilt-native".to_owned(),])
    );
}

#[test]
fn package_scan_rejects_manifest_identity_mismatch() {
    let directory = tempdir().expect("temporary directory");
    write_manifest(directory.path(), "different", "1.2.3", None);
    assert!(scan_package(directory.path(), &package("fixture", "1.2.3")).is_err());
}

#[test]
fn post_vendor_manifest_detects_package_file_and_set_changes() {
    let directory = tempdir().expect("temporary directory");
    let root = directory.path();
    write_manifest(root, "fixture", "1.2.3", None);
    fs::create_dir(root.join("src")).expect("create source directory");
    fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n").expect("write source");
    write_checksum(root, "package-checksum");

    verify_package_checksum(root, "package-checksum").expect("valid checksum fixture");

    fs::write(root.join("src/lib.rs"), "pub fn changed() {}\n").expect("change source");
    assert!(verify_package_checksum(root, "package-checksum").is_err());
    fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n").expect("restore source");

    fs::write(root.join("src/extra.rs"), "").expect("add unexpected source");
    assert!(verify_package_checksum(root, "package-checksum").is_err());
    fs::remove_file(root.join("src/extra.rs")).expect("remove unexpected source");

    fs::remove_file(root.join("src/lib.rs")).expect("remove expected source");
    assert!(verify_package_checksum(root, "package-checksum").is_err());
    fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n").expect("restore source");

    assert!(verify_package_checksum(root, "different-package-checksum").is_err());
}

#[cfg(unix)]
#[test]
fn vendored_symlink_is_rejected() {
    use std::os::unix::fs::symlink;

    let directory = tempdir().expect("temporary directory");
    fs::write(directory.path().join("source"), "content").expect("write source");
    symlink("source", directory.path().join("alias")).expect("create symlink");
    assert!(regular_files(directory.path()).is_err());
}
