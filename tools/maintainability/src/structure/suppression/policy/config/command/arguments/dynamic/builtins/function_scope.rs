use std::collections::{BTreeMap, BTreeSet};

use super::{attributes, command_word_index, integer_value_is_opaque, tokens};
use effects::{Call, Event as EffectEvent};

mod effects;
#[cfg(test)]
mod tests;

const MAX_SUMMARY_ITERATIONS: usize = 64;

pub(super) struct Dependencies {
    functions: BTreeSet<String>,
    by_function: BTreeMap<String, DependencySet>,
}

impl Dependencies {
    pub(super) fn analyze(commands: &[tokens::StructuredCommand], functions: &BTreeSet<String>) -> Self {
        let mut scopes = attributes::IntegerAttributes::default();
        let mut builders = BTreeMap::<String, Builder>::new();
        for command in commands {
            scopes.enter_scope(command, functions);
            if let Some(function) = scopes.current_function() {
                builders.entry(function.to_owned()).or_default().observe(command, functions);
            }
            scopes.leave_scope(command);
        }

        let mut by_function = builders
            .iter()
            .map(|(function, builder)| (function.clone(), builder.dependencies.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut converged = false;
        for _ in 0..MAX_SUMMARY_ITERATIONS {
            let previous = by_function.clone();
            for (function, builder) in &builders {
                by_function.insert(function.clone(), builder.resolve(&previous));
            }
            if by_function == previous {
                converged = true;
                break;
            }
        }
        if !converged {
            for summary in by_function.values_mut() {
                summary.always_opaque = true;
                summary.global_effects.clear();
            }
        }
        Self {
            functions: functions.clone(),
            by_function,
        }
    }

    pub(super) fn call_is_opaque(&self, command: &tokens::StructuredCommand, attributes: &attributes::IntegerAttributes) -> bool {
        let Some(function) = called_function(command, &self.functions) else {
            return false;
        };
        self.by_function.get(function).is_some_and(|dependencies| {
            dependencies.always_opaque
                || dependencies.dynamic.iter().any(|name| attributes.is_integer(name))
                || dependencies.global.iter().any(|name| attributes.is_global_integer(name))
        })
    }

    pub(super) fn apply_call_effects(&self, command: &tokens::StructuredCommand, attributes: &mut attributes::IntegerAttributes, uncertain: bool, persistent: bool) {
        if !persistent {
            return;
        }
        let Some(function) = called_function(command, &self.functions) else {
            return;
        };
        if let Some(summary) = self.by_function.get(function) {
            for (name, effect) in &summary.global_effects {
                attributes.apply_global_effect(name, *effect, uncertain);
            }
        }
    }
}

#[derive(Clone, Default, Eq, PartialEq)]
struct DependencySet {
    dynamic: BTreeSet<String>,
    global: BTreeSet<String>,
    always_opaque: bool,
    global_effects: BTreeMap<String, attributes::GlobalAttributeEffect>,
}

impl DependencySet {
    fn merge_relevance(&mut self, mut other: Self) {
        self.dynamic.append(&mut other.dynamic);
        self.global.append(&mut other.global);
        self.always_opaque |= other.always_opaque;
    }
}

#[derive(Default)]
struct Builder {
    locals: BTreeMap<String, LocalAttribute>,
    dependencies: DependencySet,
    effect_events: Vec<EffectEvent>,
    control_depth: usize,
    subshell_locals: Vec<BTreeMap<String, LocalAttribute>>,
    terminated: bool,
}

impl Builder {
    fn resolve(&self, known: &BTreeMap<String, DependencySet>) -> DependencySet {
        effects::evaluate(&self.effect_events, known, self.dependencies.always_opaque)
    }

    fn observe(&mut self, command: &tokens::StructuredCommand, functions: &BTreeSet<String>) {
        if self.terminated {
            return;
        }
        self.close_control_groups(command);
        for _ in 0..command.open_subshells {
            self.subshell_locals.push(self.locals.clone());
        }
        let pipeline_locals = command.isolated.then(|| self.locals.clone());
        let definitely_executed = self.control_depth == 0 && !command.conditionally_executed && !command.isolated;
        let effects_persist = self.subshell_locals.is_empty() && !command.isolated;
        let words = function_body_words(&command.words, functions);
        self.observe_ordinary_assignments(words);
        self.observe_nested_commands(&command.source, functions);
        let returns = command_word_index(words).is_some_and(|index| {
            let arguments = &words[index + 1..];
            let command_word = words[index].trim_matches(['(', ')', '{', '}', ';']);
            match command_word {
                "declare" | "local" | "typeset" => self.observe_declaration(command_word, arguments, definitely_executed, effects_persist),
                "export" | "readonly" => self.observe_preserving_assignment(command_word, arguments),
                "unset" => self.observe_unset(arguments, definitely_executed),
                "printf" | "read" | "readarray" | "mapfile" | "getopts" => self.observe_builtin_assignment(command_word, arguments),
                _ => {}
            }
            command_word == "return" && effects_persist
        });
        if let Some(function) = called_function_words(words, functions) {
            let locally_bound = self
                .locals
                .iter()
                .filter_map(|(name, attribute)| (!attribute.may_inherit()).then_some(name.clone()))
                .collect();
            let locally_integer = self.locals.iter().filter_map(|(name, attribute)| attribute.may_integer().then_some(name.clone())).collect();
            let call = Call {
                function: function.to_owned(),
                locally_bound,
                locally_integer,
            };
            self.effect_events.push(EffectEvent::Call {
                call,
                uncertain: !definitely_executed,
                effects_persist,
            });
        }
        if returns {
            self.effect_events.push(EffectEvent::Terminate { uncertain: !definitely_executed });
            self.terminated = definitely_executed;
        }
        if let Some(locals) = pipeline_locals {
            self.locals = locals;
        }
        for _ in 0..command.close_subshells {
            if let Some(locals) = self.subshell_locals.pop() {
                self.locals = locals;
            }
        }
        self.open_control_groups(command);
    }

    fn observe_ordinary_assignments(&mut self, command: &[String]) {
        let mut index = 0;
        while let Some(argument) = command.get(index) {
            let word = argument.trim_matches(['(', ')', '{', '}', ';']);
            if word.is_empty() || word == "time" || super::is_shell_command_prefix(word) {
                index += 1;
                continue;
            }
            if let Some(attached) = super::redirection_has_attached_operand(word) {
                index = super::index_after_redirection(command, index, attached).unwrap_or(command.len());
                continue;
            }
            let Some((name, value)) = attributes::assignment(word) else {
                break;
            };
            if integer_value_is_opaque(value) {
                self.record_evaluation(name, self.locals.get(name).copied().unwrap_or(LocalAttribute::INHERITED));
            }
            index += 1;
        }
    }

    fn observe_declaration(&mut self, command: &str, arguments: &[String], definitely_executed: bool, effects_persist: bool) {
        for assignment in attributes::declaration_assignments(command, arguments) {
            if assignment.force_global {
                self.observe_global_declaration(assignment, definitely_executed, effects_persist);
                continue;
            }
            let attribute = assignment
                .integer_attribute
                .map(|integer| if integer { LocalAttribute::INTEGER } else { LocalAttribute::PLAIN })
                .or_else(|| assignment.inherit.then_some(LocalAttribute::INHERITED));
            let attribute = attribute.or_else(|| self.locals.get(assignment.name).copied()).unwrap_or(LocalAttribute::PLAIN);
            if assignment.value.is_some_and(integer_value_is_opaque) {
                self.record_evaluation(assignment.name, attribute);
            }
            self.update_local(assignment.name, attribute, definitely_executed);
        }
    }

    fn observe_global_declaration(&mut self, assignment: attributes::DeclarationAssignment<'_>, definitely_executed: bool, effects_persist: bool) {
        if assignment.value.is_some_and(integer_value_is_opaque) {
            if assignment.integer_attribute == Some(true) {
                self.dependencies.always_opaque = true;
            } else if assignment.integer_attribute != Some(false) {
                self.dependencies.global.insert(assignment.name.to_owned());
                self.effect_events.push(EffectEvent::EvaluateGlobal { name: assignment.name.to_owned() });
            }
        }
        if let Some(integer) = assignment.integer_attribute {
            self.record_global_effect(assignment.name, integer, definitely_executed, effects_persist);
        }
    }

    fn record_global_effect(&mut self, name: &str, integer: bool, definitely_executed: bool, effects_persist: bool) {
        if !effects_persist {
            self.dependencies.always_opaque = true;
            return;
        }
        let effect = attributes::GlobalAttributeEffect::from_integer(integer);
        let effect = if definitely_executed {
            effect
        } else {
            attributes::GlobalAttributeEffect::IDENTITY.join(effect)
        };
        self.effect_events.push(EffectEvent::Direct { name: name.to_owned(), effect });
    }

    fn observe_preserving_assignment(&mut self, command: &str, arguments: &[String]) {
        for assignment in attributes::declaration_assignments(command, arguments) {
            if assignment.value.is_some_and(integer_value_is_opaque) {
                self.record_inherited_evaluation(assignment.name);
            }
        }
    }

    fn observe_unset(&mut self, arguments: &[String], definitely_executed: bool) {
        let mut variables = true;
        let mut options = true;
        let mut targets = Vec::new();
        for argument in arguments {
            let word = argument.trim_matches(['\'', '"', ';']);
            if options && word == "--" {
                options = false;
                continue;
            }
            let option = options && word.starts_with('-') && word != "-";
            if option && !word[1..].bytes().all(|flag| matches!(flag, b'f' | b'n' | b'v')) {
                return;
            }
            if option {
                variables &= !word[1..].contains('f');
                continue;
            }
            options = false;
            if variables && super::identifier(word).is_some() {
                targets.push(word.to_owned());
            }
        }
        for target in targets {
            self.update_local(&target, LocalAttribute::PLAIN, definitely_executed);
        }
    }

    fn update_local(&mut self, name: &str, attribute: LocalAttribute, definitely_executed: bool) {
        let attribute = if definitely_executed || self.locals.get(name) == Some(&attribute) {
            attribute
        } else {
            self.locals.get(name).copied().unwrap_or(LocalAttribute::INHERITED).join(attribute)
        };
        self.locals.insert(name.to_owned(), attribute);
    }

    fn close_control_groups(&mut self, command: &tokens::StructuredCommand) {
        if control_words(command).any(|word| matches!(word, "fi" | "done" | "esac")) {
            self.control_depth = self.control_depth.saturating_sub(1);
        }
    }

    fn open_control_groups(&mut self, command: &tokens::StructuredCommand) {
        if control_words(command).any(|word| matches!(word, "if" | "while" | "until" | "for" | "select" | "case")) {
            self.control_depth += 1;
        }
    }

    fn observe_builtin_assignment(&mut self, command: &str, arguments: &[String]) {
        let mut assigned = BTreeSet::new();
        let mut opaque = false;
        match command {
            "printf" => super::collect_printf_target(arguments, &mut assigned, &mut opaque),
            "read" | "readarray" | "mapfile" => super::collect_read_targets(command, arguments, &mut assigned, &mut opaque),
            "getopts" => super::collect_getopts_target(arguments, &mut assigned, &mut opaque),
            _ => unreachable!("matched assignment builtin"),
        }
        for name in assigned {
            self.record_evaluation(&name, self.locals.get(&name).copied().unwrap_or(LocalAttribute::INHERITED));
        }
    }

    fn record_inherited_evaluation(&mut self, name: &str) {
        self.record_evaluation(name, self.locals.get(name).copied().unwrap_or(LocalAttribute::INHERITED));
    }

    fn record_evaluation(&mut self, name: &str, attribute: LocalAttribute) {
        self.dependencies.always_opaque |= attribute.may_integer();
        if attribute.may_inherit() {
            self.dependencies.dynamic.insert(name.to_owned());
        }
        self.effect_events.push(EffectEvent::EvaluateDynamic { name: name.to_owned(), attribute });
    }

    fn observe_nested_commands(&mut self, source: &str, functions: &BTreeSet<String>) {
        let (mut nested, _) = tokens::command_substitution_commands(source, true);
        nested.extend(tokens::process_substitution_commands(source).0);
        for source in nested {
            let mut child = Self {
                locals: self.locals.clone(),
                ..Self::default()
            };
            for command in tokens::structured_source_commands(&source) {
                child.observe(&command, functions);
            }
            self.dependencies.dynamic.extend(child.dependencies.dynamic.iter().cloned());
            self.dependencies.global.extend(child.dependencies.global.iter().cloned());
            self.dependencies.always_opaque |= child.dependencies.always_opaque;
            if child.dependencies.always_opaque || !child.effect_events.is_empty() {
                self.effect_events.push(EffectEvent::Child {
                    events: child.effect_events,
                    always_opaque: child.dependencies.always_opaque,
                });
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct LocalAttribute(u8);

impl LocalAttribute {
    const PLAIN: Self = Self(1);
    const INTEGER: Self = Self(2);
    const INHERITED: Self = Self(4);

    const fn join(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    const fn may_integer(self) -> bool {
        self.0 & Self::INTEGER.0 != 0
    }

    const fn may_inherit(self) -> bool {
        self.0 & Self::INHERITED.0 != 0
    }
}

fn shell_keyword(word: &str) -> &str {
    word.trim_matches(['(', ')', '{', '}', ';'])
}

fn control_words(command: &tokens::StructuredCommand) -> impl Iterator<Item = &str> {
    let end = command_word_index(&command.words).map_or(command.words.len(), |index| index + 1);
    command.words[..end].iter().map(|word| shell_keyword(word))
}

fn called_function<'a>(command: &'a tokens::StructuredCommand, functions: &BTreeSet<String>) -> Option<&'a str> {
    if attributes::function_opener(&command.words, functions).is_some() {
        return None;
    }
    called_function_words(&command.words, functions)
}

fn called_function_words<'a>(words: &'a [String], functions: &BTreeSet<String>) -> Option<&'a str> {
    let index = command_word_index(words)?;
    if words[..index]
        .iter()
        .any(|word| matches!(word.trim_matches(['(', ')', '{', '}', ';']), "builtin" | "command" | "exec" | "nohup"))
    {
        return None;
    }
    let candidate = words[index].trim_matches(['(', ')', '{', '}', ';']);
    functions.contains(candidate).then_some(candidate)
}

fn function_body_words<'a>(words: &'a [String], functions: &BTreeSet<String>) -> &'a [String] {
    attributes::function_opener(words, functions)
        .and_then(|_| words.iter().position(|word| word == "{").map(|brace| &words[brace + 1..]))
        .unwrap_or(words)
}
