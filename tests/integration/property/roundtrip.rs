use localhold::server::params::{ReadResponse, RememberResponse};
use proptest::prelude::*;

use super::strategies::{arb_remember_input, fidelity_config};
use crate::helpers::{call_tool, setup_noop_server};

proptest! {
    #![proptest_config(fidelity_config())]

    /// P2: Remember-then-read roundtrip preserves full content and tags.
    #[test]
    fn remember_read_roundtrip(input in arb_remember_input()) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let client = setup_noop_server().await;

            // Extract expected values from input before sending
            let expected_content = input["content"].as_str().unwrap().to_owned();
            let expected_tags: Vec<String> = input["tags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect();

            let remember_resp: RememberResponse = call_tool(&client, "remember", input).await;
            assert!(!remember_resp.id.to_string().is_empty(), "remember must return a non-empty ID");

            let read: ReadResponse = call_tool(&client, "read", serde_json::json!({"id": remember_resp.id})).await;
            assert_eq!(read.memory.content, expected_content, "roundtrip content mismatch");
            assert_eq!(read.memory.tags, expected_tags, "roundtrip tags mismatch");
        });
    }
}
