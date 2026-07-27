use std::fs;

use tempfile::{TempDir, tempdir};

use super::{SiteKind, scan_workspace};

const AUDITED_ROOTS: [&str; 3] = ["src", "tests", "benches"];

fn test_workspace() -> TempDir {
    let workspace = tempdir().expect("temporary workspace");
    for root in AUDITED_ROOTS {
        fs::create_dir(workspace.path().join(root)).expect("tracked root");
    }
    workspace
}

fn scan_test_workspace(workspace: &TempDir) -> anyhow::Result<Vec<super::UnsafeSite>> {
    scan_workspace(workspace.path(), &AUDITED_ROOTS.map(str::to_owned))
}

fn scan_result(source: &str) -> anyhow::Result<Vec<super::UnsafeSite>> {
    let workspace = test_workspace();
    fs::write(workspace.path().join("src/sample.rs"), source).expect("sample source");
    scan_test_workspace(&workspace)
}

fn scan(source: &str) -> Vec<super::UnsafeSite> {
    scan_result(source).expect("scan succeeds")
}

fn assert_source_expansion_rejected(source: &str) {
    let error = scan_result(source).expect_err("source expansion must fail closed");
    assert!(error.to_string().contains("unsupported or unaudited Rust source construct"), "unexpected error: {error:#}");
}

fn assert_unsafe_macro_rejected(source: &str) {
    let error = scan_result(source).expect_err("macro-generated unsafe syntax must fail closed");
    assert!(error.to_string().contains("unsupported or unaudited Rust source construct"), "unexpected error: {error:#}");
}

#[test]
fn rejects_root_build_script() {
    let workspace = test_workspace();
    fs::write(workspace.path().join("build.rs"), "fn main() {}").expect("root build script");

    let error = scan_test_workspace(&workspace).expect_err("root build script must fail closed");
    assert!(error.to_string().contains("root build.rs is not supported"), "unexpected error: {error:#}");
}

#[test]
fn scans_optional_examples_root() {
    let workspace = test_workspace();
    fs::create_dir(workspace.path().join("examples")).expect("examples root");
    fs::write(workspace.path().join("examples/sample.rs"), "fn sample() { unsafe { work() } }").expect("example source");

    let sites = scan_test_workspace(&workspace).expect("scan succeeds");
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].path, "examples/sample.rs");
    assert_eq!(sites[0].kind, SiteKind::Block);
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_source_root() {
    use std::os::unix::fs::symlink;

    let workspace = test_workspace();
    fs::remove_dir(workspace.path().join("src")).expect("remove tracked root");
    fs::create_dir(workspace.path().join("real-src")).expect("symlink target");
    symlink(workspace.path().join("real-src"), workspace.path().join("src")).expect("symlinked root");

    let error = scan_test_workspace(&workspace).expect_err("symlinked root must fail closed");
    assert!(error.to_string().contains("tracked source root cannot be a symlink"), "unexpected error: {error:#}");
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_source_entry() {
    use std::os::unix::fs::symlink;

    let workspace = test_workspace();
    fs::write(workspace.path().join("sample.rs"), "fn sample() {}").expect("symlink target");
    symlink(workspace.path().join("sample.rs"), workspace.path().join("src/sample.rs")).expect("symlinked entry");

    let error = scan_test_workspace(&workspace).expect_err("symlinked entry must fail closed");
    assert!(
        error.to_string().contains("tracked Rust source tree cannot contain symlinks"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn ignores_non_executable_unsafe_text_and_function_pointer_types() {
    let sites = scan(
        r#"
        const LABEL: &str = "unsafe { ignored() }";
        const POINTER: unsafe extern "C" fn() = external;
        // unsafe { ignored() }
        fn r#unsafe() {}
        "#,
    );
    assert!(sites.is_empty(), "unexpected sites: {sites:?}");
}

#[test]
fn detects_executable_forms_and_exceptions() {
    let sites = scan(
        r#"
        #[expect(unsafe_code, reason = "contract")]
        #[cfg_attr(any(), expect(unsafe_op_in_unsafe_fn, reason = "contract"))]
        #[expect(clippy::undocumented_unsafe_blocks, reason = "contract")]
        #[expect(clippy::restriction, reason = "contract")]
        #[expect(clippy::all, reason = "contract")]
        #[warn(unsafe_code)]
        #[cfg_attr(any(), warn(clippy::undocumented_unsafe_blocks))]
        unsafe fn top() { unsafe { work() } }
        unsafe trait Boundary {}
        unsafe impl Boundary for Value {}
        unsafe extern "C" { unsafe fn foreign(); }
        #[unsafe(no_mangle)]
        static mut MUTABLE: u8 = 0;
        core::arch::global_asm!("");
        "#,
    );
    let kinds: Vec<_> = sites.iter().map(|site| site.kind).collect();
    assert_eq!(kinds.iter().filter(|kind| **kind == SiteKind::LintException).count(), 7);
    assert_eq!(kinds.iter().filter(|kind| **kind == SiteKind::Function).count(), 2);
    assert!(kinds.contains(&SiteKind::Block));
    assert!(kinds.contains(&SiteKind::Trait));
    assert!(kinds.contains(&SiteKind::Impl));
    assert!(kinds.contains(&SiteKind::ExternBlock));
    assert_eq!(kinds.iter().filter(|kind| **kind == SiteKind::MacroInput).count(), 1);
    assert!(kinds.contains(&SiteKind::Attribute));
    assert!(kinds.contains(&SiteKind::MutableStatic));
}

#[test]
fn tracks_nested_item_and_occurrence() {
    let sites = scan(
        r"
        impl Store {
            fn register() {
                unsafe { first() }
                unsafe { second() }
            }
        }
        ",
    );
    assert_eq!(sites.len(), 2);
    assert!(sites.iter().all(|site| site.item == "Store::register"));
    assert_eq!(sites[0].occurrence, 0);
    assert_eq!(sites[1].occurrence, 1);
    assert_ne!(sites[0].fingerprint, sites[1].fingerprint);
}

#[test]
fn boundary_fingerprint_covers_safe_setup_around_an_operation() {
    let first = scan("fn boundary() { let pointer = first; unsafe { call(pointer) } }");
    let second = scan("fn boundary() { let pointer = second; unsafe { call(pointer) } }");
    assert_eq!(first[0].fingerprint, second[0].fingerprint);
    assert_ne!(first[0].boundary_fingerprint, second[0].boundary_fingerprint);
}

#[test]
fn rejects_source_expansion_outside_audited_files() {
    assert_source_expansion_rejected(r#"#[path = "../outside.rs"] mod outside;"#);
    assert_source_expansion_rejected(r#"#[r#path = "../outside.rs"] mod outside;"#);
    assert_source_expansion_rejected(r#"include!("../outside.rs");"#);
    assert_source_expansion_rejected(r#"r#include!("../outside.rs");"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { () => { include!("../outside.rs"); } }"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { () => { #[path = "../outside.rs"] mod outside; } }"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { ($m:ident) => { $m!("../outside.rs") } } expand!(include);"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { ($a:ident) => { #[$a = "../outside.rs"] mod outside; } } expand!(path);"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { ($a:ident ;) => { #[$a = "../outside.rs"] mod outside; } } expand!(path;);"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { ($a:ident, $m:ident) => { #[$a = "../outside.rs"] $m outside; } } expand!(path, mod);"#);
    assert_source_expansion_rejected(r#"macro_rules! expand { ($a:tt) => { # $a mod outside; } } expand!([path = "../outside.rs"]);"#);
    assert_source_expansion_rejected(r"macro_rules! expand { ($a:tt) => { #! $a } } expand!([allow(unsafe_code)]);");
    assert_source_expansion_rejected(r#"use std::include as source; source!("../outside.rs");"#);
}

#[test]
fn standalone_assembly_invocations_are_fingerprinted() {
    let first = scan(r#"core::arch::global_asm!(".byte 0");"#);
    let second = scan(r#"core::arch::global_asm!(".byte 1");"#);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].kind, SiteKind::MacroInput);
    assert_ne!(first[0].fingerprint, second[0].fingerprint);
}

#[test]
fn rejects_imported_or_aliased_assembly_macros() {
    for source in [
        r#"use std::arch::global_asm; global_asm!("");"#,
        r#"use std::arch::global_asm as assembly; assembly!("");"#,
        r#"pub use std::arch::{r#global_asm as assembly}; assembly!("");"#,
        r#"use std::arch::asm as assembly; fn sample() { unsafe { assembly!(""); } }"#,
    ] {
        let error = scan_result(source).expect_err("assembly imports must fail closed");
        assert!(error.to_string().contains("imports an assembly macro"), "unexpected error: {error:#}");
    }
}

#[test]
fn rejects_macro_generated_unsafe_constructs() {
    for source in [
        r"
        macro_rules! unsafe_wrap { ($body:block) => { unsafe $body } }
        fn sample() { unsafe_wrap!({ *pointer }); }
        ",
        r#"
        macro_rules! assemble { ($code:expr) => { core::arch::global_asm!($code); } }
        fn sample() { assemble!(".byte 0"); }
        "#,
        r#"
        macro_rules! assemble { ($code:expr) => { core::arch::asm!($code); } }
        fn sample() { unsafe { assemble!("nop"); } }
        "#,
        r"
        macro_rules! hide {
            () => { #[allow(unsafe_code)] #[allow(dead_code)] static mut VALUE: i32 = 0; }
        }
        hide!();
        ",
        r"
        #[allow(unsafe_code, dead_code)]
        mod hidden {
            macro_rules! make_static { ($m:ident) => { static $m VALUE: i32 = 0; } }
            make_static!(mut);
        }
        ",
        r"
        macro_rules! identity { ($body:block) => { $body } }
        fn sample() { identity!({ unsafe { work() } }); }
        ",
    ] {
        assert_unsafe_macro_rejected(source);
    }
}

#[test]
fn rejects_macro_invocations_inside_unsafe_blocks() {
    assert_unsafe_macro_rejected(
        r"
        macro_rules! dereference { ($pointer:expr) => { *$pointer } }
        fn sample(pointer: *const u8) { unsafe { dereference!(pointer); } }
        ",
    );
}

#[test]
fn rejects_macro_invocations_inside_unsafe_function_bodies() {
    assert_unsafe_macro_rejected(
        r"
        macro_rules! dereference { ($pointer:expr) => { *$pointer } }
        #[allow(unsafe_code, unsafe_op_in_unsafe_fn)]
        unsafe fn read(pointer: *const u8) -> u8 { dereference!(pointer) }
        ",
    );
}
