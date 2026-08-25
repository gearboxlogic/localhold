use std::collections::BTreeSet;

use super::{
    ArgumentSpec, CallScanner, called_name, is_direct_mutator, is_identifier_character, is_identifier_start, is_path_constructor, is_path_mutation_method, path_argument_is_opaque,
};

pub(super) fn has_opaque_reference(scanner: &CallScanner, invoked: &BTreeSet<usize>) -> bool {
    let safe_bound_methods = safe_bound_path_methods(scanner);
    let mut index = 0;
    while index < scanner.characters.len() {
        if scanner.characters[index] == '#' {
            index = scanner.comment_end(index);
            continue;
        }
        if let Some(literal) = scanner.string_literal(index) {
            index = literal.end;
            continue;
        }
        if !is_identifier_start(scanner.characters[index]) {
            index += 1;
            continue;
        }
        let start = index;
        let (name, end) = qualified_identifier(scanner, start);
        index = end;
        if invoked.contains(&start) || safe_bound_methods.contains(&start) {
            continue;
        }
        if is_mutator_capability(called_name(&name)) {
            return true;
        }
    }
    false
}

fn safe_bound_path_methods(scanner: &CallScanner) -> BTreeSet<usize> {
    let source = scanner.characters.iter().collect::<String>();
    let mut calls = CallScanner::new(&source);
    let mut safe = BTreeSet::new();
    while let Some(call) = calls.next() {
        if !is_path_constructor(called_name(&call.name)) {
            continue;
        }
        let Some(method) = calls.following_method_reference(&call) else {
            continue;
        };
        if receiver_fixed_method(called_name(&method.name)) && !path_argument_is_opaque(&calls, call.opening_parenthesis, ArgumentSpec::new(0, &[])) {
            safe.insert(method.start);
        }
    }
    safe
}

fn qualified_identifier(scanner: &CallScanner, start: usize) -> (String, usize) {
    let mut name = String::new();
    let mut index = start;
    loop {
        while scanner.characters.get(index).is_some_and(|character| is_identifier_character(*character)) {
            name.push(scanner.characters[index]);
            index += 1;
        }
        let dot = scanner.skip_whitespace(index);
        if scanner.characters.get(dot) != Some(&'.') {
            return (name, index);
        }
        let next = scanner.skip_whitespace(dot + 1);
        if !scanner.characters.get(next).is_some_and(|character| is_identifier_start(*character)) {
            return (name, index);
        }
        name.push('.');
        index = next;
    }
}

fn is_mutator_capability(name: &str) -> bool {
    if is_direct_mutator(name) {
        return true;
    }
    let method = name.rsplit_once('.').map_or(name, |(_, method)| method);
    is_path_mutation_method(method)
}

fn receiver_fixed_method(method: &str) -> bool {
    matches!(method, "chmod" | "lchmod" | "mkdir" | "open" | "rmdir" | "touch" | "unlink" | "write_bytes" | "write_text")
}
