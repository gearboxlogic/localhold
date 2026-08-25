use std::collections::BTreeSet;

use super::{collect_execution_inputs_with_policy, model, mutation};
use crate::structure::suppression::policy::config::command::arguments::package_json;

pub(super) fn execution_inputs(path: &str, source: &str, source_is_reviewed: bool, path_policy: Option<&mutation::PathPolicy>) -> Option<model::ExecutionInputs> {
    package_json::script_commands(path, source).map(|scripts| {
        scripts.map_or_else(
            |_| model::ExecutionInputs::from_paths((BTreeSet::default(), true)),
            |scripts| {
                model::ExecutionInputs::from_paths(collect_execution_inputs_with_policy(
                    scripts.iter().map(String::as_str),
                    true,
                    path,
                    source_is_reviewed,
                    path_policy,
                ))
            },
        )
    })
}
