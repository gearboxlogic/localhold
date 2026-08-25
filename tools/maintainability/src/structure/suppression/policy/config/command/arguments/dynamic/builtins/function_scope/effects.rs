use std::collections::{BTreeMap, BTreeSet};

use super::{DependencySet, LocalAttribute, attributes};

#[derive(Clone)]
pub(super) struct Call {
    pub(super) function: String,
    pub(super) locally_bound: BTreeSet<String>,
    pub(super) locally_integer: BTreeSet<String>,
}

#[derive(Clone)]
pub(super) enum Event {
    Direct { name: String, effect: attributes::GlobalAttributeEffect },
    EvaluateDynamic { name: String, attribute: LocalAttribute },
    EvaluateGlobal { name: String },
    Call { call: Call, uncertain: bool, effects_persist: bool },
    Child { events: Vec<Self>, always_opaque: bool },
    Terminate { uncertain: bool },
}

pub(super) fn evaluate(events: &[Event], known: &BTreeMap<String, DependencySet>, always_opaque: bool) -> DependencySet {
    evaluate_with_effects(events, known, always_opaque, BTreeMap::new())
}

fn evaluate_with_effects(
    events: &[Event],
    known: &BTreeMap<String, DependencySet>,
    always_opaque: bool,
    initial_effects: BTreeMap<String, attributes::GlobalAttributeEffect>,
) -> DependencySet {
    let mut dependencies = DependencySet {
        always_opaque,
        global_effects: initial_effects,
        ..DependencySet::default()
    };
    let mut terminated_effects = None;
    let mut reachable = true;
    for event in events {
        if !reachable {
            break;
        }
        match event {
            Event::Direct { name, effect } => compose_effect(&mut dependencies.global_effects, name, *effect),
            Event::EvaluateDynamic { name, attribute } => record_dynamic(&mut dependencies, name, *attribute),
            Event::EvaluateGlobal { name } => record_global(&mut dependencies, name),
            Event::Call { call, uncertain, effects_persist } => record_call(&mut dependencies, call, known.get(&call.function), *uncertain, *effects_persist),
            Event::Child { events, always_opaque } => {
                let mut child = evaluate_with_effects(events, known, *always_opaque, dependencies.global_effects.clone());
                child.global_effects.clear();
                dependencies.merge_relevance(child);
            }
            Event::Terminate { uncertain } => {
                join_effect_paths(&mut terminated_effects, &dependencies.global_effects);
                reachable = *uncertain;
            }
        }
    }
    if let Some(terminated) = terminated_effects {
        dependencies.global_effects = if reachable {
            joined_effects(&terminated, &dependencies.global_effects)
        } else {
            terminated
        };
    }
    dependencies.global_effects.retain(|_, effect| !effect.is_identity());
    dependencies
}

fn record_call(dependencies: &mut DependencySet, call: &Call, callee: Option<&DependencySet>, uncertain: bool, effects_persist: bool) {
    let Some(callee) = callee else {
        return;
    };
    dependencies.always_opaque |= callee.always_opaque;
    for name in &callee.dynamic {
        if call.locally_integer.contains(name) {
            dependencies.always_opaque = true;
        } else if !call.locally_bound.contains(name) {
            record_dynamic(dependencies, name, LocalAttribute::INHERITED);
        }
    }
    for name in &callee.global {
        record_global(dependencies, name);
    }
    if !effects_persist && !callee.global_effects.is_empty() {
        dependencies.always_opaque = true;
    }
    if effects_persist {
        compose_call_effects(&mut dependencies.global_effects, callee, uncertain);
    }
}

fn join_effect_paths(paths: &mut Option<BTreeMap<String, attributes::GlobalAttributeEffect>>, effects: &BTreeMap<String, attributes::GlobalAttributeEffect>) {
    *paths = Some(paths.as_ref().map_or_else(|| effects.clone(), |current| joined_effects(current, effects)));
}

fn joined_effects(
    left: &BTreeMap<String, attributes::GlobalAttributeEffect>,
    right: &BTreeMap<String, attributes::GlobalAttributeEffect>,
) -> BTreeMap<String, attributes::GlobalAttributeEffect> {
    left.keys()
        .chain(right.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|name| {
            let effect = effect_for(left, &name).join(effect_for(right, &name));
            (name, effect)
        })
        .collect()
}

fn record_dynamic(dependencies: &mut DependencySet, name: &str, attribute: LocalAttribute) {
    let effect = effect_for(&dependencies.global_effects, name);
    dependencies.always_opaque |= attribute.may_integer() || attribute.may_inherit() && effect.makes_plain_integer();
    if attribute.may_inherit() && effect.keeps_integer_integer() {
        dependencies.dynamic.insert(name.to_owned());
    }
}

fn record_global(dependencies: &mut DependencySet, name: &str) {
    let effect = effect_for(&dependencies.global_effects, name);
    dependencies.always_opaque |= effect.makes_plain_integer();
    if effect.keeps_integer_integer() {
        dependencies.global.insert(name.to_owned());
    }
}

fn compose_call_effects(effects: &mut BTreeMap<String, attributes::GlobalAttributeEffect>, callee: &DependencySet, uncertain: bool) {
    for (name, effect) in &callee.global_effects {
        let optional = attributes::GlobalAttributeEffect::IDENTITY.join(*effect);
        compose_effect(effects, name, if uncertain { optional } else { *effect });
    }
}

fn effect_for(effects: &BTreeMap<String, attributes::GlobalAttributeEffect>, name: &str) -> attributes::GlobalAttributeEffect {
    effects.get(name).copied().unwrap_or(attributes::GlobalAttributeEffect::IDENTITY)
}

fn compose_effect(effects: &mut BTreeMap<String, attributes::GlobalAttributeEffect>, name: &str, next: attributes::GlobalAttributeEffect) {
    let current = effects.get(name).copied().unwrap_or(attributes::GlobalAttributeEffect::IDENTITY);
    effects.insert(name.to_owned(), current.then(next));
}
