use super::super::evaluation;
use super::{CallScanner, called_name, imports, is_direct_mutator, is_path_mutation_method};

const REFLECTION_NAMES: &[&str] = &[
    "getattr",
    "vars",
    "__dict__",
    "__getattribute__",
    "attrgetter",
    "methodcaller",
    "getattr_static",
    "getmembers",
    "getmembers_static",
    "classify_class_attrs",
];

pub(super) fn has_opaque_capability(source: &str, canonical: &str, aliases: &imports::Aliases) -> bool {
    let mut signals = Signals::default();
    scan(source, aliases, &mut signals);
    scan(canonical, aliases, &mut signals);
    signals.reflection && signals.filesystem
}

#[derive(Default)]
struct Signals {
    reflection: bool,
    filesystem: bool,
}

fn scan(source: &str, aliases: &imports::Aliases, signals: &mut Signals) {
    let scanner = CallScanner::new(source);
    let mut index = 0;
    while index < scanner.characters.len() && !(signals.reflection && signals.filesystem) {
        if scanner.characters[index] == '#' {
            index = scanner.comment_end(index);
            continue;
        }
        if let Some(literal) = scanner.string_literal(index) {
            if literal.formatted {
                scan_formatted_expression(&scanner, &literal, aliases, signals);
            }
            index = literal.end;
            continue;
        }
        if !super::is_identifier_start(scanner.characters[index]) {
            index += 1;
            continue;
        }
        let (name, end) = qualified_identifier(&scanner, index);
        signals.reflection |= is_reflection(&name);
        signals.filesystem |= is_filesystem_capability(&name);
        index = end;
    }
}

fn scan_formatted_expression(scanner: &CallScanner, literal: &super::StringLiteral, aliases: &imports::Aliases, signals: &mut Signals) {
    let content = scanner.characters[literal.content_start..literal.content_end].iter().collect::<String>();
    let Some(expressions) = evaluation::formatted_code_expressions(&content) else {
        signals.reflection = true;
        signals.filesystem = true;
        return;
    };
    for expression in expressions {
        let Some(canonical) = aliases.canonicalize_expression(&expression) else {
            signals.reflection = true;
            signals.filesystem = true;
            return;
        };
        scan(&canonical, aliases, signals);
    }
}

pub(super) fn is_reflection(name: &str) -> bool {
    called_name(name).split('.').any(|segment| REFLECTION_NAMES.contains(&segment))
}

fn is_filesystem_capability(name: &str) -> bool {
    let canonical = called_name(name);
    let root = canonical.split('.').next().unwrap_or(canonical);
    let method = canonical.rsplit('.').next().unwrap_or(canonical);
    matches!(
        root,
        "Path" | "PosixPath" | "WindowsPath" | "_io" | "_pyio" | "io" | "nt" | "os" | "pathlib" | "posix" | "shutil" | "tempfile"
    ) || is_direct_mutator(canonical)
        || is_path_mutation_method(method)
}

fn qualified_identifier(scanner: &CallScanner, start: usize) -> (String, usize) {
    let mut name = String::new();
    let mut index = start;
    loop {
        while scanner.characters.get(index).is_some_and(|character| super::is_identifier_character(*character)) {
            name.push(scanner.characters[index]);
            index += 1;
        }
        let dot = scanner.skip_trivia(index);
        if scanner.characters.get(dot) != Some(&'.') {
            return (name, index);
        }
        let next = scanner.skip_trivia(dot + 1);
        if scanner.characters.get(next).is_none_or(|character| !super::is_identifier_start(*character)) {
            return (name, index);
        }
        name.push('.');
        index = next;
    }
}
