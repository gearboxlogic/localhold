//! Integration tests for the localhold MCP server.

mod access;
mod batch_and_count;
#[expect(unused_results, reason = "test setup: process cleanup and assertions discard results intentionally")]
mod binary_smoke;
mod chaos_extended;
mod chaos_quick;
mod chaos_standard;
mod concurrency_stress;
mod edge_cases;
mod embedding_batch_hardening;
mod embedding_races;
mod fault_injection;
#[expect(unused_results, reason = "test helpers discard intermediate results intentionally")]
#[expect(dead_code, unused_macro_rules, reason = "shared integration helpers intentionally cover multiple transport and fixture styles")]
mod helpers;
mod http_transport;
mod property;
mod protocol;
mod reembed;
mod schema_snapshots;
mod soak;
mod transport_matrix;
#[expect(unused_results, reason = "test setup and assertions discard many results intentionally")]
mod write_authorization;
