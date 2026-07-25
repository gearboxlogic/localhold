use localhold::server::params::{DeleteResponse, RememberResponse};
use proptest::prelude::*;
use serde_json::json;

use super::strategies::fidelity_config;
use crate::helpers::{assert_invalid_params_contains, call_tool, call_tool_error, setup_noop_server};

proptest! {
    #![proptest_config(fidelity_config())]

    /// P5: Forgetting a memory twice does not crash. The first delete succeeds,
    /// and the second delete returns a not-found error.
    #[test]
    fn forget_idempotency(content in "[a-zA-Z0-9][a-zA-Z0-9 ]{0,99}") {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let client = setup_noop_server().await;

            let remember_resp: RememberResponse = call_tool(
                &client,
                "remember",
                json!({"content": content, "context": {"allow_unresolved": true}}),
            )
                .await;

            let del_resp: DeleteResponse = call_tool(
                &client,
                "forget",
                json!({"id": remember_resp.id}),
            )
            .await;
            assert!(del_resp.deleted, "first delete should return deleted=true");

            let err = call_tool_error(
                &client,
                "forget",
                json!({"id": remember_resp.id}),
            )
            .await;
            assert!(
                err.contains("not found"),
                "second delete should return 'not found', got: {err}"
            );
        });
    }

    /// P6: Explicitly deferred writes remain contextless and are labeled for later classification.
    #[test]
    fn explicit_deferral_writes_to_unresolved_inbox(content in "[a-zA-Z0-9][a-zA-Z0-9 ]{0,99}") {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let client = setup_noop_server().await;

            let remember_resp: RememberResponse = call_tool(
                &client,
                "remember",
                json!({"content": content, "context": {"allow_unresolved": true}}),
            )
            .await;
            assert_eq!(remember_resp.scope, "inbox/unresolved");
            assert!(remember_resp.unresolved_scope, "missing scope should be marked unresolved");
            assert!(
                remember_resp.warnings.iter().any(|warning| warning.code == "missing_scope"),
                "missing scope should emit a quality warning"
            );

            assert_invalid_params_contains(
                &client,
                "admin_list",
                json!({"scope": "inbox/unresolved"}),
                "legacy scope has no unique governed context",
            )
            .await;
        });
    }
}
