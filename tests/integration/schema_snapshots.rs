//! Snapshot tests for MCP tool schemas and response type JSON schemas.
//!
//! These tests use `insta` to detect unintended schema drift. When a schema
//! changes, the snapshot diff surfaces exactly what moved so that the change
//! can be reviewed and explicitly accepted.

use localhold::server::params;

// ---------------------------------------------------------------------------
// Response type JSON schemas (via schemars)
// ---------------------------------------------------------------------------

#[test]
fn response_schema_tool_error_response() {
    let schema = schemars::schema_for!(params::ToolErrorResponse);
    insta::assert_json_snapshot!("response_schema_ToolErrorResponse", schema);
}

#[test]
fn response_schema_quality_warning() {
    let schema = schemars::schema_for!(params::QualityWarning);
    insta::assert_json_snapshot!("response_schema_QualityWarning", schema);
}

#[test]
fn response_schema_scope_resolution() {
    let schema = schemars::schema_for!(params::ScopeResolution);
    insta::assert_json_snapshot!("response_schema_ScopeResolution", schema);
}

#[test]
fn response_schema_duplicate_candidate_card() {
    let schema = schemars::schema_for!(params::DuplicateCandidateCard);
    insta::assert_json_snapshot!("response_schema_DuplicateCandidateCard", schema);
}

#[test]
fn response_schema_remember_response() {
    let schema = schemars::schema_for!(params::RememberResponse);
    insta::assert_json_snapshot!("response_schema_RememberResponse", schema);
}

#[test]
fn response_schema_match_quality() {
    let schema = schemars::schema_for!(params::MatchQuality);
    insta::assert_json_snapshot!("response_schema_MatchQuality", schema);
}

#[test]
fn response_schema_match_action() {
    let schema = schemars::schema_for!(params::MatchAction);
    insta::assert_json_snapshot!("response_schema_MatchAction", schema);
}

#[test]
fn response_schema_match_score_basis() {
    let schema = schemars::schema_for!(params::MatchScoreBasis);
    insta::assert_json_snapshot!("response_schema_MatchScoreBasis", schema);
}

#[test]
fn response_schema_match_assessment() {
    let schema = schemars::schema_for!(params::MatchAssessment);
    insta::assert_json_snapshot!("response_schema_MatchAssessment", schema);
}

#[test]
fn response_schema_match_diagnostics() {
    let schema = schemars::schema_for!(params::MatchDiagnostics);
    insta::assert_json_snapshot!("response_schema_MatchDiagnostics", schema);
}

#[test]
fn response_schema_recall_card() {
    let schema = schemars::schema_for!(params::RecallCard);
    insta::assert_json_snapshot!("response_schema_RecallCard", schema);
}

#[test]
fn response_schema_recall_response() {
    let schema = schemars::schema_for!(params::RecallResponse);
    insta::assert_json_snapshot!("response_schema_RecallResponse", schema);
}

#[test]
fn response_schema_read_response() {
    let schema = schemars::schema_for!(params::ReadResponse);
    insta::assert_json_snapshot!("response_schema_ReadResponse", schema);
}

#[test]
fn response_schema_read_many_response() {
    let schema = schemars::schema_for!(params::ReadManyResponse);
    insta::assert_json_snapshot!("response_schema_ReadManyResponse", schema);
}

#[test]
fn response_schema_recommended_action_tool() {
    let schema = schemars::schema_for!(params::RecommendedActionTool);
    insta::assert_json_snapshot!("response_schema_RecommendedActionTool", schema);
}

#[test]
fn response_schema_recommended_action_priority() {
    let schema = schemars::schema_for!(params::RecommendedActionPriority);
    insta::assert_json_snapshot!("response_schema_RecommendedActionPriority", schema);
}

#[test]
fn response_schema_recommended_action() {
    let schema = schemars::schema_for!(params::RecommendedAction);
    insta::assert_json_snapshot!("response_schema_RecommendedAction", schema);
}

#[test]
fn response_schema_brief_response() {
    let schema = schemars::schema_for!(params::BriefResponse);
    insta::assert_json_snapshot!("response_schema_BriefResponse", schema);
}

#[test]
fn response_schema_handoff_suggestion() {
    let schema = schemars::schema_for!(params::HandoffSuggestion);
    insta::assert_json_snapshot!("response_schema_HandoffSuggestion", schema);
}

#[test]
fn response_schema_handoff_response() {
    let schema = schemars::schema_for!(params::HandoffResponse);
    insta::assert_json_snapshot!("response_schema_HandoffResponse", schema);
}

#[test]
fn response_schema_memory_entry() {
    let schema = schemars::schema_for!(params::MemoryEntry);
    insta::assert_json_snapshot!("response_schema_MemoryEntry", schema);
}

#[test]
fn response_schema_update_response() {
    let schema = schemars::schema_for!(params::UpdateResponse);
    insta::assert_json_snapshot!("response_schema_UpdateResponse", schema);
}

#[test]
fn response_schema_delete_response() {
    let schema = schemars::schema_for!(params::DeleteResponse);
    insta::assert_json_snapshot!("response_schema_DeleteResponse", schema);
}

#[test]
fn response_schema_evict_expired_response() {
    let schema = schemars::schema_for!(params::EvictExpiredResponse);
    insta::assert_json_snapshot!("response_schema_EvictExpiredResponse", schema);
}

#[test]
fn response_schema_reassign_scope_response() {
    let schema = schemars::schema_for!(params::ReassignScopeResponse);
    insta::assert_json_snapshot!("response_schema_ReassignScopeResponse", schema);
}

#[test]
fn response_schema_count_response() {
    let schema = schemars::schema_for!(params::CountResponse);
    insta::assert_json_snapshot!("response_schema_CountResponse", schema);
}

#[test]
fn response_schema_scope_count() {
    let schema = schemars::schema_for!(params::ScopeCount);
    insta::assert_json_snapshot!("response_schema_ScopeCount", schema);
}

#[test]
fn response_schema_tag_count() {
    let schema = schemars::schema_for!(params::TagCount);
    insta::assert_json_snapshot!("response_schema_TagCount", schema);
}

#[test]
fn response_schema_agent_count() {
    let schema = schemars::schema_for!(params::AgentCount);
    insta::assert_json_snapshot!("response_schema_AgentCount", schema);
}

#[test]
fn response_schema_reembed_response() {
    let schema = schemars::schema_for!(params::ReembedResponse);
    insta::assert_json_snapshot!("response_schema_ReembedResponse", schema);
}

#[test]
fn response_schema_admin_migrate_metadata_response() {
    let schema = schemars::schema_for!(params::AdminMigrateMetadataResponse);
    insta::assert_json_snapshot!("response_schema_AdminMigrateMetadataResponse", schema);
}

// ---------------------------------------------------------------------------
// MCP tool schemas (via list_all_tools over the protocol)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mcp_tool_schemas() {
    let client = super::helpers::setup_noop_server().await;
    let mut tools = client.list_all_tools().await.unwrap();

    // Sort by name for deterministic snapshot order.
    tools.sort_by(|a, b| a.name.cmp(&b.name));

    // Snapshot each tool individually so diffs are scoped to one tool.
    for tool in &tools {
        insta::assert_json_snapshot!(format!("mcp_tool_schema_{}", tool.name), tool);
    }
}
