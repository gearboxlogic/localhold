//! Shared constants and helpers for concurrency stress scenarios.

use localhold::types::{AccessPolicy, Memory, Provenance};

/// Number of concurrent tasks for quick (default) tests.
pub(crate) const QUICK_TASKS: usize = 4;

/// Number of operations per task for quick tests.
pub(crate) const QUICK_OPS: usize = 10;

/// Number of concurrent tasks for standard tests.
pub(crate) const STANDARD_TASKS: usize = 16;

/// Number of operations per task for standard tests.
pub(crate) const STANDARD_OPS: usize = 100;

/// Build a minimal test memory with a unique ID and the given content.
pub(crate) fn build_memory(content: &str, agent: &str) -> Memory {
    Memory::new_for_test(
        content.to_owned(),
        Vec::new(),
        Provenance::new_for_test(Some(agent.to_owned()), None, None),
        AccessPolicy::Public,
    )
}

/// Build a test memory with conversation scope fields set.
pub(crate) fn build_scoped_memory(content: &str, agent: &str, scope: &str) -> Memory {
    Memory::new_for_test(
        content.to_owned(),
        Vec::new(),
        Provenance::new_for_test(Some(agent.to_owned()), Some(scope.to_owned()), Some(scope.to_owned())),
        AccessPolicy::Public,
    )
}
