use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default)]
pub(super) struct IntegerAttributes {
    global: BTreeMap<String, IntegerState>,
    functions: Vec<FunctionAttributes>,
    brace_depth: usize,
    pending_function: Option<String>,
}

#[derive(Clone)]
struct FunctionAttributes {
    name: String,
    values: BTreeMap<String, IntegerState>,
    closing_depth: usize,
}

impl IntegerAttributes {
    pub(super) fn enter_scope(&mut self, command: &super::tokens::StructuredCommand, functions: &BTreeSet<String>) {
        let inline = (command.open_braces > 0).then(|| function_opener(&command.words, functions)).flatten();
        let pending = (command.open_braces > 0 && command.words.first().is_some_and(|word| word == "{"))
            .then(|| self.pending_function.take())
            .flatten();
        if let Some(name) = inline.map(str::to_owned).or(pending) {
            let closing_depth = self.brace_depth;
            self.brace_depth += command.open_braces;
            self.functions.push(FunctionAttributes {
                name,
                values: BTreeMap::new(),
                closing_depth,
            });
            self.pending_function = None;
        } else {
            self.brace_depth += command.open_braces;
            if !command.words.is_empty() {
                self.pending_function = (command.open_braces == 0).then(|| function_name(&command.words, functions).map(str::to_owned)).flatten();
            }
        }
    }

    pub(super) fn leave_scope(&mut self, command: &super::tokens::StructuredCommand) {
        for _ in 0..command.close_braces {
            self.brace_depth = self.brace_depth.saturating_sub(1);
            while self.functions.last().is_some_and(|scope| scope.closing_depth == self.brace_depth) {
                self.functions.pop();
            }
        }
    }

    pub(super) fn is_integer(&self, name: &str) -> bool {
        self.functions
            .iter()
            .rev()
            .find_map(|scope| scope.values.get(name))
            .or_else(|| self.global.get(name))
            .copied()
            .unwrap_or(IntegerState::Plain)
            .may_be_integer()
    }

    pub(super) fn is_global_integer(&self, name: &str) -> bool {
        self.global.get(name).copied().unwrap_or(IntegerState::Plain).may_be_integer()
    }

    pub(super) fn current_function(&self) -> Option<&str> {
        self.functions.last().map(|scope| scope.name.as_str())
    }

    pub(super) fn apply_global_effect(&mut self, name: &str, effect: GlobalAttributeEffect, uncertain: bool) {
        let effect = if uncertain { GlobalAttributeEffect::IDENTITY.join(effect) } else { effect };
        let current = self.global.get(name).copied().unwrap_or(IntegerState::Plain);
        self.global.insert(name.to_owned(), effect.apply(current));
    }

    pub(super) fn update_declaration(&mut self, command: &str, arguments: &[String], uncertain: bool) {
        for assignment in declaration_assignments(command, arguments) {
            self.set_declaration(assignment, uncertain);
        }
    }

    pub(super) fn declaration_initializer_is_opaque(&self, command: &str, arguments: &[String]) -> bool {
        declaration_assignments(command, arguments).into_iter().any(|assignment| {
            assignment
                .value
                .is_some_and(|value| self.declared_integer(&assignment) && super::integer_value_is_opaque(value))
        })
    }

    pub(super) fn preserving_initializer_is_opaque(&self, command: &str, arguments: &[String]) -> bool {
        declaration_assignments(command, arguments).into_iter().any(|assignment| {
            assignment
                .value
                .is_some_and(|value| self.is_integer(assignment.name) && super::integer_value_is_opaque(value))
        })
    }

    pub(super) fn update_unset(&mut self, arguments: &[String], uncertain: bool) {
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
                targets.push(word);
            }
        }
        for target in targets {
            self.clear(target, uncertain);
        }
    }

    pub(super) fn ordinary_assignment_is_opaque(&self, command: &[String]) -> bool {
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
            let Some((name, value)) = assignment(word) else {
                break;
            };
            if self.is_integer(name) && super::integer_value_is_opaque(value) {
                return true;
            }
            index += 1;
        }
        false
    }

    fn set_declaration(&mut self, assignment: DeclarationAssignment<'_>, uncertain: bool) {
        if assignment.force_global {
            if let Some(integer) = assignment.integer_attribute {
                self.update_global(assignment.name, IntegerState::from(integer), uncertain || !self.functions.is_empty());
            }
            return;
        }
        if self.functions.is_empty() {
            if let Some(integer) = assignment.integer_attribute {
                self.update_global(assignment.name, IntegerState::from(integer), uncertain);
            }
            return;
        }
        let inherited = self.state(assignment.name);
        let state = assignment.integer_attribute.map(IntegerState::from).or_else(|| assignment.inherit.then_some(inherited));
        let values = &mut self.functions.last_mut().expect("function scope is active").values;
        let new = state.unwrap_or_else(|| values.get(assignment.name).copied().unwrap_or(IntegerState::Plain));
        let current = values.get(assignment.name).copied().unwrap_or(inherited);
        values.insert(assignment.name.to_owned(), if uncertain { current.join(new) } else { new });
    }

    fn declared_integer(&self, assignment: &DeclarationAssignment<'_>) -> bool {
        if assignment.force_global || self.functions.is_empty() {
            return assignment.integer_attribute.unwrap_or_else(|| self.is_global_integer(assignment.name));
        }
        assignment
            .integer_attribute
            .map(IntegerState::from)
            .or_else(|| assignment.inherit.then(|| self.state(assignment.name)))
            .or_else(|| self.functions.last().and_then(|scope| scope.values.get(assignment.name)).copied())
            .unwrap_or(IntegerState::Plain)
            .may_be_integer()
    }

    fn clear(&mut self, name: &str, uncertain: bool) {
        if let Some(scope) = self.functions.iter_mut().rev().find(|scope| scope.values.contains_key(name)) {
            let current = scope.values.get(name).copied().unwrap_or(IntegerState::Plain);
            scope
                .values
                .insert(name.to_owned(), if uncertain { current.join(IntegerState::Plain) } else { IntegerState::Plain });
        } else if self.global.contains_key(name) {
            self.update_global(name, IntegerState::Plain, uncertain || !self.functions.is_empty());
        }
    }

    fn state(&self, name: &str) -> IntegerState {
        self.functions
            .iter()
            .rev()
            .find_map(|scope| scope.values.get(name))
            .or_else(|| self.global.get(name))
            .copied()
            .unwrap_or(IntegerState::Plain)
    }

    fn update_global(&mut self, name: &str, new: IntegerState, uncertain: bool) {
        let current = self.global.get(name).copied().unwrap_or(IntegerState::Plain);
        self.global.insert(name.to_owned(), if uncertain { current.join(new) } else { new });
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum IntegerState {
    Plain,
    Integer,
    MaybeInteger,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GlobalAttributeEffect(u8);

impl GlobalAttributeEffect {
    pub(super) const IDENTITY: Self = Self(0b_10_01);
    const SET_PLAIN: Self = Self(0b_01_01);
    const SET_INTEGER: Self = Self(0b_10_10);

    pub(super) const fn from_integer(integer: bool) -> Self {
        if integer { Self::SET_INTEGER } else { Self::SET_PLAIN }
    }

    pub(super) const fn join(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(super) const fn then(self, next: Self) -> Self {
        let plain = outputs_after(self.0 & 0b11, next);
        let integer = outputs_after((self.0 >> 2) & 0b11, next);
        Self(plain | integer << 2)
    }

    pub(super) const fn is_identity(self) -> bool {
        self.0 == Self::IDENTITY.0
    }

    pub(super) const fn makes_plain_integer(self) -> bool {
        self.0 & 0b10 != 0
    }

    pub(super) const fn keeps_integer_integer(self) -> bool {
        self.0 & 0b_1000 != 0
    }

    const fn apply(self, state: IntegerState) -> IntegerState {
        let outputs = match state {
            IntegerState::Plain => self.0 & 0b11,
            IntegerState::Integer => (self.0 >> 2) & 0b11,
            IntegerState::MaybeInteger => (self.0 & 0b11) | ((self.0 >> 2) & 0b11),
        };
        match outputs {
            0b01 => IntegerState::Plain,
            0b10 => IntegerState::Integer,
            _ => IntegerState::MaybeInteger,
        }
    }
}

const fn outputs_after(inputs: u8, effect: GlobalAttributeEffect) -> u8 {
    let mut outputs = 0;
    if inputs & 0b01 != 0 {
        outputs |= effect.0 & 0b11;
    }
    if inputs & 0b10 != 0 {
        outputs |= (effect.0 >> 2) & 0b11;
    }
    outputs
}

impl IntegerState {
    const fn may_be_integer(self) -> bool {
        !matches!(self, Self::Plain)
    }

    const fn join(self, other: Self) -> Self {
        if matches!((self, other), (Self::Plain, Self::Integer) | (Self::Integer, Self::Plain)) || matches!(self, Self::MaybeInteger) || matches!(other, Self::MaybeInteger) {
            Self::MaybeInteger
        } else {
            self
        }
    }
}

impl From<bool> for IntegerState {
    fn from(integer: bool) -> Self {
        if integer { Self::Integer } else { Self::Plain }
    }
}

#[derive(Clone, Copy)]
pub(super) struct DeclarationAssignment<'a> {
    pub(super) name: &'a str,
    pub(super) value: Option<&'a str>,
    pub(super) integer_attribute: Option<bool>,
    pub(super) force_global: bool,
    pub(super) inherit: bool,
}

struct DeclarationOptions {
    active: bool,
    integer_attribute: Option<bool>,
    force_global: bool,
    inherit: bool,
}

pub(super) fn declaration_assignments<'a>(command: &str, arguments: &'a [String]) -> Vec<DeclarationAssignment<'a>> {
    let mut options = DeclarationOptions::default();
    let mut assignments = Vec::new();
    for argument in arguments {
        let word = argument.trim_matches(['\'', '"', ';']);
        if options.consume(command, word) {
            continue;
        }
        let (target, value) = word.split_once('=').map_or((word, None), |(target, value)| (target, Some(value)));
        let target = target.strip_suffix('+').unwrap_or(target);
        let Some(name) = super::identifier(target) else {
            continue;
        };
        assignments.push(DeclarationAssignment {
            name,
            value,
            integer_attribute: options.integer_attribute,
            force_global: options.force_global,
            inherit: options.inherit,
        });
    }
    assignments
}

impl Default for DeclarationOptions {
    fn default() -> Self {
        Self {
            active: true,
            integer_attribute: None,
            force_global: false,
            inherit: false,
        }
    }
}

impl DeclarationOptions {
    fn consume(&mut self, command: &str, word: &str) -> bool {
        if self.active && word == "--" {
            self.active = false;
            return true;
        }
        if !self.active || word.len() <= 1 || !matches!(word.as_bytes()[0], b'-' | b'+') {
            return false;
        }
        if word[1..].contains('i') {
            self.integer_attribute = Some(word.starts_with('-'));
        }
        self.inherit |= word.starts_with('-') && word[1..].contains('I');
        self.force_global |= command != "local" && word.starts_with('-') && word[1..].contains('g');
        true
    }
}

pub(super) fn function_opener<'a>(command: &'a [String], functions: &BTreeSet<String>) -> Option<&'a str> {
    let brace = command.iter().position(|word| word == "{")?;
    function_name(&command[..brace], functions)
}

fn function_name<'a>(command: &'a [String], functions: &BTreeSet<String>) -> Option<&'a str> {
    let name = match command {
        [name] => name.strip_suffix("()"),
        [name, parentheses] if parentheses == "()" => Some(name.as_str()),
        [name, open, close] if open == "(" && close == ")" => Some(name.as_str()),
        [keyword, name] if keyword == "function" => Some(name.trim_end_matches("()")),
        [keyword, name, parentheses] if keyword == "function" && parentheses == "()" => Some(name.as_str()),
        _ => None,
    };
    name.filter(|name| functions.contains(*name))
}

pub(super) fn assignment(word: &str) -> Option<(&str, &str)> {
    let (target, value) = word.split_once('=')?;
    let target = target.strip_suffix('+').unwrap_or(target);
    let name = target.split_once('[').map_or(target, |(name, _)| name);
    super::identifier(name).map(|name| (name, value))
}
