use proptest::prelude::*;
use serde_json::{Value, json};

/// Read `PROPTEST_FIDELITY` env var and return the corresponding case count.
pub(crate) fn fidelity_cases() -> u32 {
    let raw = std::env::var("PROPTEST_FIDELITY").unwrap_or_default();
    match raw.as_str() {
        "extended" => 1000_u32,
        "standard" => 100_u32,
        // "quick" or unset
        _ => 10_u32,
    }
}

/// Build a proptest `Config` with the fidelity-aware case count.
pub(crate) fn fidelity_config() -> proptest::test_runner::Config {
    let cases = fidelity_cases();
    proptest::test_runner::Config { cases, ..Default::default() }
}

/// Generate a non-empty string of printable ASCII characters (length 1..=`max_len`).
fn arb_nonempty_string(max_len: usize) -> BoxedStrategy<String> {
    "[a-zA-Z0-9 _-]{1,}"
        .prop_filter_map("string must be 1..=max_len", move |s| {
            let trimmed = s.trim().to_owned();
            if trimmed.is_empty() || trimmed.len() > max_len { None } else { Some(trimmed) }
        })
        .boxed()
}

/// Generate a valid `AccessPolicy` as `serde_json::Value`.
pub(crate) fn arb_access_policy() -> BoxedStrategy<Value> {
    let public = Just(json!({"type": "public"}));

    let restricted = prop::collection::vec(arb_nonempty_string(30), 1_usize..=3_usize).prop_map(|allowed| json!({"type": "restricted", "allowed": allowed}));

    let visible_options: &[&str] = &["content", "tags", "provenance", "entities", "importance"];
    let redacted = prop::sample::subsequence(visible_options, 0_usize..=5_usize).prop_map(|fields| {
        let field_values: Vec<Value> = fields.into_iter().map(|f| json!(f)).collect();
        json!({"type": "redacted", "visible_fields": field_values})
    });

    prop_oneof![
        3 => public,
        3 => restricted,
        3 => redacted,
    ]
    .boxed()
}

/// Generate a `serde_json::Value` representing valid v2 `remember` parameters.
pub(crate) fn arb_remember_input() -> BoxedStrategy<Value> {
    let content = arb_nonempty_string(1000);
    let tags = prop::collection::vec(arb_nonempty_string(50), 0_usize..=5_usize);
    let scope = prop::option::of(arb_nonempty_string(50));
    let agent_label = prop::option::of(arb_nonempty_string(50));
    let summary = prop::option::of(arb_nonempty_string(140));
    let importance = prop::option::of(0.0_f64..=1.0_f64);
    let confidence = prop::option::of(0.0_f64..=1.0_f64);
    let access_policy = prop::option::of(arb_access_policy());

    (content, tags, scope, agent_label, summary, importance, confidence, access_policy)
        .prop_map(|(content, tags, scope, agent_label, summary, importance, confidence, policy)| {
            let mut obj = json!({
                "content": content,
                "tags": tags,
            });
            if let Some(scope) = scope {
                obj["scope"] = json!(scope);
            }
            if let Some(label) = agent_label {
                obj["agent_label"] = json!(label);
            }
            if let Some(summary) = summary {
                obj["summary"] = json!(summary);
            }
            if let Some(importance) = importance {
                obj["importance"] = json!(importance);
            }
            if let Some(confidence) = confidence {
                obj["confidence"] = json!(confidence);
            }
            if let Some(p) = policy {
                obj["access_policy"] = p;
            }
            obj
        })
        .boxed()
}

/// Generate a `serde_json::Value` for v2 recall/admin-list filter fields.
#[expect(dead_code, reason = "strategy available for future property tests")]
pub(crate) fn arb_memory_filter() -> BoxedStrategy<Value> {
    let tags = prop::option::of(prop::collection::vec(arb_nonempty_string(50), 1_usize..=3_usize));
    let scope = prop::option::of(arb_nonempty_string(50));

    (tags, scope)
        .prop_map(|(tags, scope)| {
            let mut obj = json!({});
            if let Some(t) = tags {
                obj["tags"] = json!(t);
            }
            if let Some(scope) = scope {
                obj["scope"] = json!(scope);
            }
            obj
        })
        .boxed()
}

/// Generate an `Option<f64>` in range `0.0..=10.0` suitable for `max_distance`.
#[expect(dead_code, reason = "strategy available for future property tests")]
pub(crate) fn arb_max_distance() -> BoxedStrategy<Option<f64>> {
    prop::option::of(0.0_f64..=10.0_f64).boxed()
}
