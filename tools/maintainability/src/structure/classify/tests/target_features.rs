use super::*;

#[test]
fn binaries_requiring_the_isolated_testing_feature_are_test_only() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[features]\ntesting = []\ncli = []\n\
         \n[[bin]]\nname = \"test-support\"\npath = \"src/test_support.rs\"\nrequired-features = [\"testing\"]\n\
         \n[[bin]]\nname = \"cli\"\npath = \"src/cli.rs\"\nrequired-features = [\"cli\"]\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/lib.rs"), "fn root() {}\n").expect("root source");
    fs::write(repository.path().join("src/test_support.rs"), "fn main() { SqliteStore::open(); }\n").expect("test-support binary");
    fs::write(repository.path().join("src/cli.rs"), "fn main() { PostgresStore::open(); }\n").expect("production binary");

    let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace inventory");
    let by_path = inventory.files.iter().map(|file| (file.path.as_str(), file)).collect::<BTreeMap<_, _>>();
    assert_eq!(by_path["src/test_support.rs"].production_lines, 0);
    assert_eq!(by_path["src/test_support.rs"].production_concrete_stores.sqlite_store, 0);
    assert_eq!(by_path["src/test_support.rs"].production_concrete_stores.postgres_store, 0);
    assert_eq!(by_path["src/cli.rs"].production_lines, 1);
    assert_eq!(by_path["src/cli.rs"].production_concrete_stores.postgres_store, 1);
}

#[test]
fn a_testing_binary_cannot_make_a_shared_library_root_test_only() {
    let repository = tempfile::tempdir().expect("temporary repository");
    for root in ["src", "tests", "benches"] {
        fs::create_dir(repository.path().join(root)).expect("source root");
    }
    fs::write(
        repository.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\
         \n[features]\ntesting = []\n\
         \n[lib]\npath = \"src/shared.rs\"\n\
         \n[[bin]]\nname = \"test-support\"\npath = \"src/shared.rs\"\nrequired-features = [\"testing\"]\n",
    )
    .expect("package manifest");
    fs::write(repository.path().join("src/shared.rs"), "fn open() { SqliteStore::open(); }\n").expect("shared target source");

    let inventory = scan_workspace(repository.path(), &["src".to_owned(), "tests".to_owned(), "benches".to_owned()]).expect("workspace inventory");
    let shared = inventory.files.iter().find(|file| file.path == "src/shared.rs").expect("shared target measurement");
    assert_eq!(shared.production_lines, 1);
    assert_eq!(shared.production_concrete_stores.sqlite_store, 1);
}
