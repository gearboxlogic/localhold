use super::*;

#[test]
fn composition_roots_cannot_generate_nested_restricted_visibility_macros() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(repository.path().join("Cargo.toml"), "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n").expect("package manifest");
    fs::write(
        repository.path().join("src/main.rs"),
        "macro_rules! outer {\n\
             () => {\n\
                 macro_rules! generated { () => { pub(crate) struct Generated; } }\n\
                 generated!();\n\
                 generated!();\n\
             }\n\
         }\n\
         outer!();\n\
         fn main() {}\n",
    )
    .expect("composition source");

    let error = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()])
        .expect_err("composition roots must use the same nested visibility macro guard");
    assert!(error.to_string().contains("cannot define nested macros"), "{error:#}");
}
