use std::collections::BTreeSet;

pub(in crate::structure::suppression::policy::config::command) struct ExecutionInputs {
    pub(in crate::structure::suppression::policy::config::command) paths: BTreeSet<String>,
    pub(in crate::structure::suppression::policy::config::command) windows_bare_programs: BTreeSet<String>,
    pub(in crate::structure::suppression::policy::config::command) unresolved: bool,
}

impl ExecutionInputs {
    pub(super) fn from_paths((paths, unresolved): (BTreeSet<String>, bool)) -> Self {
        Self {
            paths,
            windows_bare_programs: BTreeSet::new(),
            unresolved,
        }
    }
}
