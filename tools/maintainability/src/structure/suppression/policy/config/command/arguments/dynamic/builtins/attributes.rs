use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct IntegerAttributes {
    global: BTreeMap<String, bool>,
    functions: Vec<FunctionAttributes>,
    brace_depth: usize,
    pending_function: bool,
}

struct FunctionAttributes {
    values: BTreeMap<String, bool>,
    closing_depth: usize,
}

impl IntegerAttributes {
    pub(super) fn enter_scope(&mut self, command: &super::tokens::StructuredCommand, functions: &BTreeSet<String>) {
        let opens_function =
            command.open_braces > 0 && (function_opener(&command.words, functions) || self.pending_function && command.words.first().is_some_and(|word| word == "{"));
        if opens_function {
            let closing_depth = self.brace_depth;
            self.brace_depth += command.open_braces;
            self.functions.push(FunctionAttributes {
                values: BTreeMap::new(),
                closing_depth,
            });
            self.pending_function = false;
        } else {
            self.brace_depth += command.open_braces;
            if !command.words.is_empty() {
                self.pending_function = command.open_braces == 0 && function_name(&command.words, functions).is_some();
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
            .unwrap_or(false)
    }

    pub(super) fn update_declaration(&mut self, command: &str, arguments: &[String]) {
        let mut options = DeclarationOptions::default();
        for argument in arguments {
            let word = argument.trim_matches(['\'', '"', ';']);
            if options.consume(command, word) {
                continue;
            }
            let target = word.split_once('=').map_or(word, |(target, _)| target);
            let target = target.strip_suffix('+').unwrap_or(target);
            let Some(name) = super::identifier(target) else {
                continue;
            };
            self.set_declaration(name, options.integer_attribute, options.force_global, options.inherit);
        }
    }

    pub(super) fn update_unset(&mut self, arguments: &[String]) {
        let mut variables = true;
        let mut options = true;
        for argument in arguments {
            let word = argument.trim_matches(['\'', '"', ';']);
            if options && word == "--" {
                options = false;
                continue;
            }
            if options && word.starts_with('-') && word != "-" {
                variables &= !word[1..].contains('f');
                continue;
            }
            if variables && super::identifier(word).is_some() {
                self.clear(word);
            }
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

    fn set_declaration(&mut self, name: &str, integer_attribute: Option<bool>, force_global: bool, inherit: bool) {
        if force_global || self.functions.is_empty() {
            if let Some(integer) = integer_attribute {
                self.global.insert(name.to_owned(), integer);
            }
            return;
        }
        let integer_attribute = integer_attribute.or_else(|| inherit.then(|| self.is_integer(name)));
        let values = &mut self.functions.last_mut().expect("function scope is active").values;
        match integer_attribute {
            Some(integer) => {
                values.insert(name.to_owned(), integer);
            }
            None => {
                values.entry(name.to_owned()).or_insert(false);
            }
        }
    }

    fn clear(&mut self, name: &str) {
        if let Some(scope) = self.functions.iter_mut().rev().find(|scope| scope.values.contains_key(name)) {
            scope.values.insert(name.to_owned(), false);
        } else if self.global.contains_key(name) {
            self.global.insert(name.to_owned(), false);
        }
    }
}

struct DeclarationOptions {
    active: bool,
    integer_attribute: Option<bool>,
    force_global: bool,
    inherit: bool,
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

fn function_opener(command: &[String], functions: &BTreeSet<String>) -> bool {
    let Some(brace) = command.iter().position(|word| word == "{") else {
        return false;
    };
    function_name(&command[..brace], functions).is_some()
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

fn assignment(word: &str) -> Option<(&str, &str)> {
    let (target, value) = word.split_once('=')?;
    let target = target.strip_suffix('+').unwrap_or(target);
    let name = target.split_once('[').map_or(target, |(name, _)| name);
    super::identifier(name).map(|name| (name, value))
}
