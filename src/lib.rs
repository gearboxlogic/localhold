//! `LocalHold` — a semantic memory MCP server backed by SQLite and sqlite-vec.

pub(crate) mod background_tasks;
pub mod clock;
pub mod config;
pub(crate) mod consolidation;
pub mod doctor;
pub mod embedding;
pub mod engine;
pub mod error;
pub(crate) mod fusion;
pub(crate) mod http_auth;
pub mod http_transport;
pub(crate) mod ordering;
pub mod reranker;
pub(crate) mod scoring;
pub mod server;
pub mod store;
pub mod types;
pub mod ui;
pub(crate) mod validation;
