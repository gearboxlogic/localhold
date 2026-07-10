use super::*;
use crate::{
    config::{LimitsConfig, SearchConfig},
    types::{AccessPolicy, Memory, Provenance},
};

#[test]
fn standard_tool_profile_removes_admin_routes() {
    let store = crate::store::SqliteStore::in_memory().unwrap();
    let server = RecallServer::new(store, Arc::new(crate::embedding::NoopEmbedding::new()), LimitsConfig::default(), SearchConfig::default());

    for name in ADMIN_TOOLS {
        assert!(server.tool_router.get(name).is_none(), "admin route should be removed: {name}");
    }
    for name in ["brief", "recall", "read", "remember", "revise", "forget"] {
        assert!(server.tool_router.get(name).is_some(), "standard route should remain: {name}");
    }

    let server = server.with_admin_tools();
    for name in ADMIN_TOOLS {
        assert!(server.tool_router.get(name).is_some(), "explicit admin profile should add route: {name}");
    }
}

// ---------------------------------------------------------------------------
// Unit tests for expand_scope_hierarchy
// ---------------------------------------------------------------------------

#[test]
fn expand_scope_hierarchy_three_segments() {
    let result = expand_scope_hierarchy("org/project/conv");
    assert_eq!(result, vec!["org/project/conv", "org/project", "org"]);
}

#[test]
fn expand_scope_hierarchy_two_segments() {
    let result = expand_scope_hierarchy("org/project");
    assert_eq!(result, vec!["org/project", "org"]);
}

#[test]
fn expand_scope_hierarchy_single_segment() {
    let result = expand_scope_hierarchy("org");
    assert_eq!(result, vec!["org"]);
}

#[test]
fn expand_scope_hierarchy_empty_string() {
    let result = expand_scope_hierarchy("");
    assert!(result.is_empty(), "empty scope should produce empty expansion");
}

#[test]
fn expand_scope_keys_deduplicates() {
    // Two scopes sharing ancestors should not produce duplicates
    let result = expand_scope_keys(&["org/project/conv-1".into(), "org/project/conv-2".into()]);
    // Should contain: org/project/conv-1, org/project, org, org/project/conv-2
    // org/project and org appear only once
    assert_eq!(result.len(), 4);
    assert_eq!(result[0], "org/project/conv-1");
    assert_eq!(result[1], "org/project");
    assert_eq!(result[2], "org");
    assert_eq!(result[3], "org/project/conv-2");
}

fn search_result_for_match_score(retrieval_score: Option<f64>, reranker_score: Option<f64>, query_relevance: Option<f64>) -> crate::types::SearchResult {
    let memory = Memory::new_for_test("match scoring candidate".into(), vec![], Provenance::default(), AccessPolicy::Public);
    crate::types::SearchResult {
        memory,
        distance: Some(0.25_f64),
        retrieval_score,
        reranker_score,
        composite_score: Some(42.0_f64),
        score_breakdown: query_relevance.map(|query_relevance| crate::types::ScoreBreakdown {
            query_relevance,
            importance: 0.5_f64,
            freshness: 0.5_f64,
            activity: 0.0_f64,
            confidence: 0.8_f64,
        }),
    }
}

#[test]
fn v2_match_assessment_prefers_final_query_relevance() {
    let result = search_result_for_match_score(Some(0.95_f64), Some(0.01_f64), Some(0.292_f64));
    let assessment = v2_match_assessment(&result, 0.7_f64);

    assert_eq!(assessment.score_basis, MatchScoreBasis::RerankerBlend);
    assert_eq!(assessment.quality, MatchQuality::Possible);
    assert_eq!(assessment.action, MatchAction::Consider);
    assert!((assessment.score - 0.292_f64).abs() < f64::EPSILON);
}

#[test]
fn v2_match_assessment_falls_back_to_retrieval_score() {
    let result = search_result_for_match_score(Some(0.75_f64), None, None);
    let assessment = v2_match_assessment(&result, 0.7_f64);

    assert_eq!(assessment.score_basis, MatchScoreBasis::Retrieval);
    assert_eq!(assessment.quality, MatchQuality::Strong);
    assert_eq!(assessment.action, MatchAction::Read);
    assert!((assessment.score - 0.75_f64).abs() < f64::EPSILON);
}
