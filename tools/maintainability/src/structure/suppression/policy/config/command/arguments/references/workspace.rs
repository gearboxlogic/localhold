use std::collections::BTreeSet;
use std::path::Path;

use super::{execution_inputs_for_surface_with_policy, model, mutation};

#[derive(Clone, Copy)]
pub(in crate::structure::suppression::policy::config::command) struct Context<'a> {
    pub(in crate::structure::suppression::policy::config::command) root: &'a Path,
    pub(in crate::structure::suppression::policy::config::command) execution_surfaces: &'a BTreeSet<String>,
    pub(in crate::structure::suppression::policy::config::command) tracked_paths: &'a BTreeSet<String>,
}

pub(in crate::structure::suppression::policy::config::command) struct Analyzer {
    policy: mutation::PathPolicy,
}

impl Analyzer {
    pub(in crate::structure::suppression::policy::config::command) fn new(context: Context<'_>) -> Self {
        Self {
            policy: mutation::PathPolicy::for_workspace(context.root, context.execution_surfaces, context.tracked_paths),
        }
    }

    pub(in crate::structure::suppression::policy::config::command) fn execution_inputs(&self, path: &str, source: &str, source_is_reviewed: bool) -> model::ExecutionInputs {
        execution_inputs_for_surface_with_policy(path, source, source_is_reviewed, Some(&self.policy))
    }
}

pub(in crate::structure::suppression::policy::config::command) fn execution_inputs_for_surface_in_workspace(
    context: Context<'_>,
    path: &str,
    source: &str,
    source_is_reviewed: bool,
) -> model::ExecutionInputs {
    Analyzer::new(context).execution_inputs(path, source, source_is_reviewed)
}
