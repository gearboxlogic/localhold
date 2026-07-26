use std::fs;

use serde_json::json;
use tempfile::tempdir;

use super::{AuditConfig, Classification, ClassificationPolicy, valid_source_id};

const CHECKSUM: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn entry() -> serde_json::Value {
    json!({
        "classification": "pure-rust-unchecked",
        "rationale": "Reviewed source exposure.",
        "review_issue": 124
    })
}

fn write_policy(directory: &std::path::Path, triggers: &serde_json::Value) {
    fs::write(
        directory.join("policy.json"),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "owner": "maintainers",
            "review_triggers": triggers,
            "packages": {}
        }))
        .expect("serialize policy settings"),
    )
    .expect("write policy settings");
}

fn required_triggers() -> serde_json::Value {
    json!(["version-or-checksum-change", "enabled-feature-change", "new-dependency-route", "exposure-signal-change"])
}

fn fragment(id: &str) -> serde_json::Value {
    let mut packages = serde_json::Map::new();
    packages.insert(id.to_owned(), entry());
    json!({"schema_version": 1, "packages": packages})
}

fn matrix() -> serde_json::Value {
    json!({
        "schema_version": 1,
        "cargo_version": "1",
        "rustc_version": "1",
        "platforms": [
            {
                "name": "linux",
                "target": "x86_64-unknown-linux-gnu",
                "baseline": "policy/dependency-unsafe/baseline/linux",
                "configurations": [{
                    "id": "linux-default",
                    "profile": "dev",
                    "features": [],
                    "include_dev": false
                }]
            },
            {
                "name": "windows",
                "target": "x86_64-pc-windows-msvc",
                "baseline": "policy/dependency-unsafe/baseline/windows",
                "configurations": [{
                    "id": "windows-default",
                    "profile": "dev",
                    "features": [],
                    "include_dev": false
                }]
            }
        ]
    })
}

#[test]
fn matrix_rejects_feature_mode_ambiguity() {
    let mut value = matrix();
    value["platforms"][0]["configurations"][0]["profile"] = json!("release");
    value["platforms"][0]["configurations"][0]["all_features"] = json!(true);
    value["platforms"][0]["configurations"][0]["features"] = json!(["feature"]);
    let config: AuditConfig = serde_json::from_value(value).expect("valid JSON shape");
    assert!(config.validate().is_err());
}

#[test]
fn matrix_rejects_artifact_path_traversal() {
    let mut value = matrix();
    value["platforms"][0]["baseline"] = json!("../../outside");
    let config: AuditConfig = serde_json::from_value(value).expect("valid JSON shape");
    assert!(config.validate().is_err());
}

#[test]
fn matrix_rejects_unsafe_configuration_filename() {
    let mut value = matrix();
    value["platforms"][0]["configurations"][0]["id"] = json!("linux-../outside");
    let config: AuditConfig = serde_json::from_value(value).expect("valid JSON shape");
    assert!(config.validate().is_err());
}

#[test]
fn normalized_source_ids_require_exact_lowercase_checksum() {
    assert!(valid_source_id(&format!("crates.io:fixture@1.2.3#{CHECKSUM}")));
    assert!(!valid_source_id("registry:fixture@1.2.3#checksum"));
    assert!(!valid_source_id("crates.io:fixture@1.2.3#short"));
    assert!(!valid_source_id(&format!("crates.io:fixture@1.2.3#{}", CHECKSUM.to_ascii_uppercase())));
}

#[test]
fn classification_fragments_merge_and_preserve_metadata() {
    let directory = tempdir().expect("temporary directory");
    write_policy(directory.path(), &required_triggers());
    let first = format!("crates.io:first@1.0.0#{CHECKSUM}");
    let second = format!("crates.io:second@2.0.0#{}", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    fs::write(directory.path().join("e-h.json"), serde_json::to_vec(&fragment(&first)).expect("serialize first fragment")).expect("write first fragment");
    fs::write(
        directory.path().join("q-t.json"),
        serde_json::to_vec(&fragment(&second)).expect("serialize second fragment"),
    )
    .expect("write second fragment");

    let policy = ClassificationPolicy::load(directory.path()).expect("load fragments");
    assert_eq!(policy.classification(&first), Some(Classification::PureRustUnchecked));
    assert_eq!(policy.packages.len(), 2);
}

#[test]
fn classification_in_wrong_alphabetical_bucket_is_rejected() {
    let directory = tempdir().expect("temporary directory");
    write_policy(directory.path(), &required_triggers());
    let id = format!("crates.io:fixture@1.0.0#{CHECKSUM}");
    let bytes = serde_json::to_vec(&fragment(&id)).expect("serialize fragment");
    fs::write(directory.path().join("q-t.json"), &bytes).expect("write misplaced fragment");
    assert!(ClassificationPolicy::load(directory.path()).is_err());
}

#[test]
fn classification_bucket_matching_is_case_insensitive() {
    let directory = tempdir().expect("temporary directory");
    write_policy(directory.path(), &required_triggers());
    let id = format!("crates.io:Inflector@1.0.0#{CHECKSUM}");
    fs::write(directory.path().join("i-l.json"), serde_json::to_vec(&fragment(&id)).expect("serialize uppercase package")).expect("write uppercase package");
    ClassificationPolicy::load(directory.path()).expect("uppercase package uses lowercase bucket");
}

#[test]
fn unsupported_policy_entries_are_not_silently_ignored() {
    let directory = tempdir().expect("temporary directory");
    fs::write(directory.path().join("classifications.jsn"), "{}").expect("write typo");
    assert!(ClassificationPolicy::load(directory.path()).is_err());
}

#[test]
fn incomplete_review_metadata_is_rejected() {
    let directory = tempdir().expect("temporary directory");
    write_policy(directory.path(), &required_triggers());
    let id = format!("crates.io:fixture@1.0.0#{CHECKSUM}");
    let mut value = fragment(&id);
    value["packages"][&id]["rationale"] = json!("");
    fs::write(directory.path().join("e-h.json"), serde_json::to_vec(&value).expect("serialize invalid fragment")).expect("write invalid fragment");
    assert!(ClassificationPolicy::load(directory.path()).is_err());
}

#[test]
fn incomplete_or_unknown_review_trigger_set_is_rejected() {
    let directory = tempdir().expect("temporary directory");
    let id = format!("crates.io:fixture@1.0.0#{CHECKSUM}");
    write_policy(directory.path(), &json!(["version-or-checksum-change", "unknown-trigger"]));
    let value = fragment(&id);
    fs::write(directory.path().join("e-h.json"), serde_json::to_vec(&value).expect("serialize invalid fragment")).expect("write invalid fragment");
    assert!(ClassificationPolicy::load(directory.path()).is_err());
}
