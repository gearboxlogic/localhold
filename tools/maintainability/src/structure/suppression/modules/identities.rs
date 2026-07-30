use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) fn propagate(roots: &BTreeMap<String, BTreeSet<String>>, relations: &BTreeMap<(String, String), String>) -> BTreeMap<String, BTreeSet<String>> {
    let mut children = BTreeMap::<&str, BTreeSet<&str>>::new();
    for ((parent, _), child) in relations {
        children.entry(parent).or_default().insert(child);
    }
    let mut identities = roots.clone();
    let mut pending = roots.keys().cloned().collect::<VecDeque<_>>();
    while let Some(parent) = pending.pop_front() {
        let Some(parent_identities) = identities.get(&parent).cloned() else {
            continue;
        };
        for child in children.get(parent.as_str()).into_iter().flatten() {
            let child_identities = identities.entry((*child).to_owned()).or_default();
            let previous_len = child_identities.len();
            child_identities.extend(parent_identities.iter().cloned());
            if child_identities.len() != previous_len {
                pending.push_back((*child).to_owned());
            }
        }
    }
    identities
}

pub(super) fn component(identities: &BTreeSet<String>) -> String {
    let encoded = identities.iter().map(|identity| format!("{}:{identity}", identity.len())).collect::<Vec<_>>().join("|");
    format!("cargo-target[{encoded}]")
}
