use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct IntegerAttributes {
    global: BTreeMap<String, bool>,
    functions: Vec<FunctionAttributes>,
    brace_depth: usize,
}

struct FunctionAttributes {
    values: BTreeMap<String, bool>,
    closing_depth: usize,
}

impl IntegerAttributes {
    pub(super) fn enter_scope(&mut self, command: &[String], functions: &BTreeSet<String>) {
        if function_opener(command, functions) {
            let closing_depth = self.brace_depth;
            self.brace_depth += 1;
            self.functions.push(FunctionAttributes {
                values: BTreeMap::new(),
                closing_depth,
            });
        } else {
            self.brace_depth += command.iter().filter(|word| word.as_str() == "{").count();
        }
    }

    pub(super) fn leave_scope(&mut self, command: &[String]) {
        for _ in 0..command.iter().filter(|word| word.as_str() == "}").count() {
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
            self.set_declaration(name, options.integer_attribute, options.force_global);
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

    fn set_declaration(&mut self, name: &str, integer_attribute: Option<bool>, force_global: bool) {
        if force_global || self.functions.is_empty() {
            if let Some(integer) = integer_attribute {
                self.global.insert(name.to_owned(), integer);
            }
            return;
        }
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
}

struct DeclarationOptions {
    active: bool,
    integer_attribute: Option<bool>,
    force_global: bool,
}

impl Default for DeclarationOptions {
    fn default() -> Self {
        Self {
            active: true,
            integer_attribute: None,
            force_global: false,
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
        self.force_global |= command != "local" && word.starts_with('-') && word[1..].contains('g');
        true
    }
}

fn function_opener(command: &[String], functions: &BTreeSet<String>) -> bool {
    let Some(brace) = command.iter().position(|word| word == "{") else {
        return false;
    };
    let name = match &command[..brace] {
        [name] => name.strip_suffix("()"),
        [name, parentheses] if parentheses == "()" => Some(name.as_str()),
        [name, open, close] if open == "(" && close == ")" => Some(name.as_str()),
        [keyword, name] if keyword == "function" => Some(name.trim_end_matches("()")),
        [keyword, name, parentheses] if keyword == "function" && parentheses == "()" => Some(name.as_str()),
        _ => None,
    };
    name.is_some_and(|name| functions.contains(name))
}

fn assignment(word: &str) -> Option<(&str, &str)> {
    let (target, value) = word.split_once('=')?;
    let target = target.strip_suffix('+').unwrap_or(target);
    let name = target.split_once('[').map_or(target, |(name, _)| name);
    super::identifier(name).map(|name| (name, value))
}
