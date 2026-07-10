use std::sync::Arc;

use localhold::{
    config::{AnonymousPolicy, LimitsConfig, SearchConfig},
    embedding::NoopEmbedding,
    engine::RecallEngine,
    server::{RecallServer, params::ReadResponse},
    store::{MemoryWriter as _, SqliteStore},
    types::{AccessPolicy, Memory, MemoryId, Provenance},
};
use proptest::prelude::*;
use rmcp::{ServiceExt as _, service::RunningService};

use super::strategies::fidelity_config;
use crate::helpers::{call_tool, call_tool_error};

/// Generate an arbitrary non-empty agent name for access policy tests.
fn arb_agent_name() -> BoxedStrategy<String> {
    "[a-zA-Z][a-zA-Z0-9_-]{0,19}".prop_filter("agent name must not be empty", |s| !s.trim().is_empty()).boxed()
}

/// Generate a list of 1-3 unique agent names for the `allowed` list.
fn arb_allowed_list() -> BoxedStrategy<Vec<String>> {
    prop::collection::vec(arb_agent_name(), 1_usize..=3_usize).boxed()
}

async fn setup_seeded_server(principal: Option<&str>, access_policy: AccessPolicy, content: &str) -> (RunningService<rmcp::RoleClient, ()>, MemoryId) {
    let store = SqliteStore::in_memory().unwrap();
    let provenance = Provenance::new_for_test(Some("owner-agent".to_owned()), None, None);
    let memory = Memory::new_for_test(content.to_owned(), Vec::new(), provenance, access_policy);
    let id = store.store(&memory, None).await.unwrap();

    let engine = RecallEngine::new(store, Arc::new(NoopEmbedding::new()), LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine_with_auth(engine, principal.map(ToOwned::to_owned), AnonymousPolicy::PublicReadOnly).with_admin_tools();

    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let _server_task = tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });
    let client = ().serve(client_transport).await.unwrap();
    (client, id)
}

proptest! {
    #![proptest_config(fidelity_config())]

    /// P3a: Public memories return full content for any trusted principal or anonymous public reader.
    #[test]
    fn public_memories_visible_to_all(caller in prop::option::of(arb_agent_name())) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (client, id) = setup_seeded_server(caller.as_deref(), AccessPolicy::Public, "public property test").await;

            let read: ReadResponse = call_tool(&client, "read", serde_json::json!({"id": id})).await;
            assert_eq!(read.memory.content, "public property test", "public memory should be visible to any caller");
        });
    }

    /// P3b: Owner always has full access on restricted memories.
    #[test]
    fn restricted_owner_always_has_access(allowed in arb_allowed_list()) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (client, id) = setup_seeded_server(
                Some("owner-agent"),
                AccessPolicy::Restricted { allowed },
                "restricted owner test",
            )
            .await;

            let read: ReadResponse = call_tool(
                &client,
                "read",
                serde_json::json!({"id": id}),
            )
            .await;
            assert_eq!(read.memory.content, "restricted owner test", "owner should always see restricted memory");
        });
    }

    /// P3c: Non-allowed principals are denied restricted memories.
    #[test]
    fn restricted_non_allowed_denied(
        allowed in arb_allowed_list(),
        intruder in arb_agent_name().prop_filter("intruder must not be owner", |s| s != "owner-agent"),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Filter out the intruder from the allowed list to ensure they are truly not allowed
            let filtered_allowed: Vec<String> = allowed.into_iter().filter(|a| a != &intruder).collect();
            // If filtering removed everything, use a single known-good agent
            let final_allowed = if filtered_allowed.is_empty() {
                vec!["definitely-not-intruder".to_owned()]
            } else {
                filtered_allowed
            };

            let (client, id) = setup_seeded_server(
                Some(&intruder),
                AccessPolicy::Restricted { allowed: final_allowed },
                "restricted denied test",
            )
            .await;

            let err = call_tool_error(
                &client,
                "read",
                serde_json::json!({"id": id}),
            )
            .await;
            assert!(
                err.contains("not found"),
                "non-allowed caller should see 'not found' for restricted memory, got: {err}"
            );
        });
    }
}
