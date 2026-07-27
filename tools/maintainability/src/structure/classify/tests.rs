use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use super::{measure_sources, physical_line_count, scan_revision, scan_workspace};

fn inventory(sources: &[(&str, &str)]) -> super::Inventory {
    let sources = sources.iter().map(|(path, source)| ((*path).to_owned(), (*source).to_owned())).collect::<BTreeMap<_, _>>();
    measure_sources(sources).expect("fixture inventory")
}

#[test]
fn physical_lines_are_lf_crlf_and_final_newline_stable() {
    assert_eq!(physical_line_count(""), 0);
    assert_eq!(physical_line_count("one"), 1);
    assert_eq!(physical_line_count("one\n"), 1);
    assert_eq!(physical_line_count("one\r\ntwo\r\n"), 2);
}

#[test]
fn cfg_test_and_testing_feature_items_are_not_production() {
    let inventory = inventory(&[(
        "src/lib.rs",
        "fn production() {}\n\
         #[cfg(test)]\nfn test_only() {}\n\
         #[cfg(any(test, feature = \"testing\"))]\nfn harness_only() {}\n\
         #[cfg(all(unix, test))]\nfn unix_test_only() {}\n\
         #[cfg(feature = \"other\")]\nfn optional_production() {}\n",
    )]);
    assert_eq!(inventory.files[0].physical_lines, 9);
    assert_eq!(inventory.files[0].production_lines, 3);
    assert_eq!(inventory.files[0].test_lines, 6);
}

#[test]
fn cfg_attr_is_test_only_only_when_it_cannot_exist_in_production() {
    let inventory = inventory(&[(
        "src/lib.rs",
        "#[cfg_attr(all(), cfg(test))]\nfn always_test() {}\n\
         #[cfg_attr(feature = \"other\", cfg(test))]\nfn sometimes_production() {}\n",
    )]);
    assert_eq!(inventory.files[0].test_lines, 2);
    assert_eq!(inventory.files[0].production_lines, 2);
}

#[test]
fn cfg_test_on_nested_syntax_is_classified() {
    let inventory = inventory(&[(
        "src/lib.rs",
        "enum Mode {\n    Live,\n    #[cfg(test)]\n    Test,\n}\n\
         impl Mode {\n    #[cfg(test)]\n    fn fixture() {}\n}\n\
         fn choose(value: Mode) {\n    match value {\n        Mode::Live => {}\n        #[cfg(test)]\n        Mode::Test => {}\n    }\n}\n",
    )]);
    let file = &inventory.files[0];
    assert_eq!(file.physical_lines, file.production_lines + file.test_lines);
    assert_eq!(file.test_lines, 6);
}

#[test]
fn external_modules_reachable_only_from_tests_are_wholly_test_only() {
    let inventory = inventory(&[
        ("src/lib.rs", "#[cfg(test)]\nmod tests;\nfn production() {}\n"),
        ("src/tests.rs", "mod nested;\nfn helper() {}\n"),
        ("src/tests/nested.rs", "fn nested_helper() {}\n"),
    ]);
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/lib.rs"].production_lines, 1);
    assert_eq!(by_path["src/tests.rs"].production_lines, 0);
    assert_eq!(by_path["src/tests/nested.rs"].production_lines, 0);
}

#[test]
fn any_production_module_edge_keeps_the_target_in_production() {
    let inventory = inventory(&[
        ("src/lib.rs", "#[cfg(not(test))]\nmod shared;\n#[cfg(test)]\nmod shared;\nfn production() {}\n"),
        ("src/shared.rs", "fn shared() {}\n"),
    ]);
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/shared.rs"].production_lines, 1);
}

#[test]
fn integration_and_benchmark_roots_are_wholly_test_only() {
    let inventory = inventory(&[("benches/load.rs", "fn benchmark_helper() {}\n"), ("tests/contract.rs", "fn integration_helper() {}\n")]);
    assert_eq!(inventory.files.len(), 2);
    assert!(inventory.files.iter().all(|file| file.production_lines == 0));
}

#[test]
fn production_cargo_targets_outside_src_are_rejected() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"escape\"\npath = \"tests/escape.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");
    fs::write(repository.path().join("tests/escape.rs"), "fn main() {}\n").expect("escaped binary");

    let error = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).unwrap_err();
    assert!(error.to_string().contains("production target must remain under src/"));
}

#[test]
fn declared_test_and_bench_targets_under_src_are_test_only() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[test]]\nname = \"custom-test\"\npath = \"src/custom_test.rs\"\n\
         \n[[bench]]\nname = \"custom-bench\"\npath = \"src/custom_bench.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");
    fs::write(repository.path().join("src/custom_test.rs"), "fn test_target() {}\n").expect("test target");
    fs::write(repository.path().join("src/custom_bench.rs"), "fn bench_target() {}\n").expect("bench target");
    fs::write(repository.path().join("src/unreferenced.rs"), "fn production_fallback() {}\n").expect("unreferenced source");

    let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace inventory");
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/custom_test.rs"].production_lines, 0);
    assert_eq!(by_path["src/custom_bench.rs"].production_lines, 0);
    assert_eq!(by_path["src/unreferenced.rs"].production_lines, 1);
}

#[test]
fn revision_scan_honors_declared_test_targets_under_src() {
    let repository = tempfile::tempdir().expect("temporary repository");
    fs::create_dir(repository.path().join("src")).expect("source root");
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[[test]]\nname = \"custom-test\"\npath = \"src/custom_test.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");
    fs::write(repository.path().join("src/custom_test.rs"), "fn test_target() {}\n").expect("test target");
    git(repository.path(), &["init", "-q"]);
    git(repository.path(), &["add", "."]);
    git(
        repository.path(),
        &["-c", "user.name=LocalHold", "-c", "user.email=localhold@example.invalid", "commit", "-q", "-m", "fixture"],
    );
    let revision = String::from_utf8(git_output(repository.path(), &["rev-parse", "HEAD"]))
        .expect("UTF-8 revision")
        .trim()
        .to_owned();

    let inventory = scan_revision(repository.path(), &revision, &["src".to_owned()]).expect("revision inventory");
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/custom_test.rs"].production_lines, 0);
}

#[test]
fn rust_examples_and_auxiliary_targets_outside_the_inventory_are_rejected() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches", "examples", "fixtures"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\n[[test]]\nname = \"escape\"\npath = \"fixtures/escape.rs\"\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");
    fs::write(repository.path().join("fixtures/escape.rs"), "fn escaped_test() {}\n").expect("escaped test target");
    let roots = ["src".to_owned(), "tests".to_owned(), "benches".to_owned()];

    let error = scan_workspace(repository.path(), &roots).unwrap_err();
    assert!(error.to_string().contains("outside the structural source inventory"));

    fs::write(repository.path().join("Cargo.toml"), "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n").expect("package manifest");
    fs::write(repository.path().join("examples/demo.rs"), "fn main() {}\n").expect("example source");
    let error = scan_workspace(repository.path(), &roots).unwrap_err();
    assert!(error.to_string().contains("untracked examples/"));
}

#[test]
fn explicit_and_conditional_module_paths_fail_closed() {
    for declaration in [
        "#[path = \"../tests/helper.rs\"]\nmod helper;\n",
        "#[cfg_attr(unix, path = \"../tests/helper.rs\")]\nmod helper;\n",
    ] {
        let sources = [
            ("src/lib.rs".to_owned(), declaration.to_owned()),
            ("tests/helper.rs".to_owned(), "fn helper() {}\n".to_owned()),
        ]
        .into_iter()
        .collect();
        assert!(measure_sources(sources).unwrap_err().to_string().contains("module paths cannot be classified safely"));
    }
}

#[test]
fn revision_scan_preserves_special_paths_and_reads_blobs_in_one_batch() {
    let repository = tempfile::tempdir().expect("temporary repository");
    fs::create_dir(repository.path().join("src")).expect("source directory");
    fs::write(repository.path().join("Cargo.toml"), "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n").expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");
    fs::write(repository.path().join("src/café space.rs"), "fn special() {}\n").expect("special source");
    git(repository.path(), &["init", "-q"]);
    git(repository.path(), &["add", "."]);
    git(
        repository.path(),
        &["-c", "user.name=LocalHold", "-c", "user.email=localhold@example.invalid", "commit", "-q", "-m", "fixture"],
    );
    let revision = String::from_utf8(git_output(repository.path(), &["rev-parse", "HEAD"]))
        .expect("UTF-8 revision")
        .trim()
        .to_owned();

    let inventory = scan_revision(repository.path(), &revision, &["src".to_owned()]).expect("revision inventory");
    let paths = inventory.files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>();
    assert_eq!(paths, ["src/café space.rs", "src/lib.rs"]);
}

fn git(repository: &Path, arguments: &[&str]) {
    let status = Command::new("git").current_dir(repository).args(arguments).status().expect("run git fixture command");
    assert!(status.success(), "git fixture command failed: {arguments:?}");
}

fn git_output(repository: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git").current_dir(repository).args(arguments).output().expect("run git fixture query");
    assert!(output.status.success(), "git fixture query failed: {arguments:?}");
    output.stdout
}
