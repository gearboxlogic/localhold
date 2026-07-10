use std::sync::Arc;

use localhold::{
    config::{AnonymousPolicy, LimitsConfig, SearchConfig},
    embedding::NoopEmbedding,
    engine::RecallEngine,
    server::{
        RecallServer,
        params::{BulkDeleteResponse, DeleteResponse, UpdateResponse},
    },
    store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
    types::{AccessPolicy, Memory, Provenance, RedactableField},
};
use rmcp::{ServiceExt as _, service::RunningService};
use serde_json::json;

use super::helpers::{call_tool, call_tool_error, setup_noop_server_with_auth};

fn policy(policy: &str) -> AccessPolicy {
    match policy {
        "public" => AccessPolicy::Public,
        "redacted" => AccessPolicy::Redacted {
            visible_fields: vec![RedactableField::Content],
        },
        "restricted" => AccessPolicy::Restricted {
            allowed: vec!["friend".to_owned()],
        },
        #[expect(clippy::panic, reason = "test helper: unreachable for known policy values")]
        other => panic!("unknown policy: {other}"),
    }
}

async fn setup_seeded_server(principal: &str, access_policy: AccessPolicy, content: String) -> (RunningService<rmcp::RoleClient, ()>, SqliteStore, localhold::types::MemoryId) {
    let store = SqliteStore::in_memory().unwrap();
    let provenance = Provenance::new_for_test(Some("owner".to_owned()), None, None);
    let memory = Memory::new_for_test(content, Vec::new(), provenance, access_policy);
    let id = store.store(&memory, None).await.unwrap();

    let engine = RecallEngine::new(store.clone(), Arc::new(NoopEmbedding::new()), LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine_with_auth(engine, Some(principal.to_owned()), AnonymousPolicy::PublicReadOnly).with_admin_tools();

    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });
    let client = ().serve(client_transport).await.unwrap();
    (client, store, id)
}

#[tokio::test]
async fn admin_bulk_delete_authorization_uses_server_resolved_principal() {
    let scenarios = [("owner", true), ("other", false)];

    for (idx, (principal, expected)) in scenarios.into_iter().enumerate() {
        let content = format!("bulk-delete-public-{principal}-{idx}");
        let (client, store, id) = setup_seeded_server(principal, AccessPolicy::Public, content.clone()).await;

        let response: BulkDeleteResponse = call_tool(
            &client,
            "admin_bulk_delete",
            json!({
                "text_search": content
            }),
        )
        .await;
        assert_eq!(response.matched, 1, "principal={principal}");

        if expected {
            assert_eq!(response.deleted, 1, "principal={principal}");
            let memory = store.get(&id, Some("owner")).await.unwrap();
            assert!(memory.is_none(), "principal={principal}");
        } else {
            assert_eq!(response.deleted, 0, "principal={principal}");
            let memory = store.get(&id, Some("owner")).await.unwrap().unwrap();
            assert_eq!(memory.content, content);
        }
    }
}

#[tokio::test]
async fn revise_authorization_matrix_uses_server_resolved_principal() {
    let scenarios = [
        ("public", "owner", true),
        ("public", "friend", false),
        ("public", "other", false),
        ("redacted", "owner", true),
        ("redacted", "friend", false),
        ("redacted", "other", false),
        ("restricted", "owner", true),
        ("restricted", "friend", true),
        ("restricted", "other", false),
    ];

    for (idx, (policy_name, principal, expected)) in scenarios.into_iter().enumerate() {
        let before = format!("before-{policy_name}-{principal}-{idx}");
        let after = format!("after-{policy_name}-{principal}-{idx}");
        let (client, store, id) = setup_seeded_server(principal, policy(policy_name), before.clone()).await;

        if expected {
            let update: UpdateResponse = call_tool(
                &client,
                "revise",
                json!({
                    "id": id,
                    "content": after
                }),
            )
            .await;
            assert!(update.updated, "policy={policy_name}, principal={principal}");

            let memory = store.get(&id, Some("owner")).await.unwrap().unwrap();
            assert_eq!(memory.content, after);
        } else {
            let err = call_tool_error(
                &client,
                "revise",
                json!({
                    "id": id,
                    "content": after
                }),
            )
            .await;
            assert!(err.contains("access denied"), "policy={policy_name}, principal={principal}: {err}");

            let memory = store.get(&id, Some("owner")).await.unwrap().unwrap();
            assert_eq!(memory.content, before);
        }
    }
}

#[tokio::test]
async fn forget_authorization_matrix_uses_server_resolved_principal() {
    let scenarios = [
        ("public", "owner", true),
        ("public", "friend", false),
        ("public", "other", false),
        ("redacted", "owner", true),
        ("redacted", "friend", false),
        ("redacted", "other", false),
        ("restricted", "owner", true),
        ("restricted", "friend", true),
        ("restricted", "other", false),
    ];

    for (idx, (policy_name, principal, expected)) in scenarios.into_iter().enumerate() {
        let content = format!("delete-{policy_name}-{principal}-{idx}");
        let (client, store, id) = setup_seeded_server(principal, policy(policy_name), content.clone()).await;

        if expected {
            let delete: DeleteResponse = call_tool(&client, "forget", json!({"id": id})).await;
            assert!(delete.deleted, "policy={policy_name}, principal={principal}");

            let memory = store.get(&id, Some("owner")).await.unwrap();
            assert!(memory.is_none(), "policy={policy_name}, principal={principal}");
        } else {
            let err = call_tool_error(&client, "forget", json!({"id": id})).await;
            assert!(err.contains("access denied"), "policy={policy_name}, principal={principal}: {err}");

            let memory = store.get(&id, Some("owner")).await.unwrap().unwrap();
            assert_eq!(memory.content, content);
        }
    }
}

#[tokio::test]
async fn revise_nonexistent_memory_returns_not_found() {
    let client = setup_noop_server_with_auth(Some("owner"), AnonymousPolicy::PublicReadOnly).await;
    let fake_id = "01J0000000000000000000000A";

    let err = call_tool_error(
        &client,
        "revise",
        json!({
            "id": fake_id,
            "content": "new content"
        }),
    )
    .await;
    assert!(err.contains("not found"), "expected not-found error, got: {err}");
}

#[tokio::test]
async fn forget_nonexistent_memory_returns_not_found() {
    let client = setup_noop_server_with_auth(Some("owner"), AnonymousPolicy::PublicReadOnly).await;
    let fake_id = "01J0000000000000000000000A";

    let err = call_tool_error(&client, "forget", json!({"id": fake_id})).await;
    assert!(err.contains("not found"), "expected not-found error, got: {err}");
}
