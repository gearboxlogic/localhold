use std::fs;

use tempfile::tempdir;

use super::{SiteKind, scan_workspace};

fn scan_result(source: &str) -> anyhow::Result<Vec<super::UnsafeSite>> {
    let workspace = tempdir().expect("temporary workspace");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(workspace.path().join(root)).expect("tracked root");
    }
    fs::write(workspace.path().join("src/sample.rs"), source).expect("sample source");
    scan_workspace(workspace.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()])
}

fn scan(source: &str) -> Vec<super::UnsafeSite> {
    scan_result(source).expect("scan succeeds")
}

fn assert_source_expansion_rejected(source: &str) {
    let error = scan_result(source).expect_err("source expansion must fail closed");
    assert!(error.to_string().contains("unsupported Rust source inclusion"), "unexpected error: {error:#}");
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
        macro_rules! generated { () => { unsafe { work() } } }
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
    assert_eq!(kinds.iter().filter(|kind| **kind == SiteKind::MacroInput).count(), 2);
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
fn tracks_imported_unsafe_assembly_macros() {
    let sites = scan(r#"use std::arch::global_asm as assembly; assembly!("");"#);
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].kind, SiteKind::MacroInput);
}

#[test]
fn tracks_macro_generated_lint_exceptions_and_mutable_statics() {
    let sites = scan(
        r"
        macro_rules! hide {
            () => { #[allow(unsafe_code)] #[allow(dead_code)] static mut VALUE: i32 = 0; }
        }
        hide!();
        ",
    );
    let kinds: Vec<_> = sites.iter().map(|site| site.kind).collect();
    assert_eq!(kinds.iter().filter(|kind| **kind == SiteKind::LintException).count(), 1);
    assert_eq!(kinds.iter().filter(|kind| **kind == SiteKind::MacroInput).count(), 1);
}

#[test]
fn tracks_static_macro_fragments_before_mutability_is_substituted() {
    let sites = scan(
        r"
        #[allow(unsafe_code, dead_code)]
        mod hidden {
            macro_rules! make_static { ($m:ident) => { static $m VALUE: i32 = 0; } }
            make_static!(mut);
        }
        ",
    );
    let kinds: Vec<_> = sites.iter().map(|site| site.kind).collect();
    assert_eq!(kinds.iter().filter(|kind| **kind == SiteKind::LintException).count(), 1);
    assert_eq!(kinds.iter().filter(|kind| **kind == SiteKind::MacroInput).count(), 1);
}
