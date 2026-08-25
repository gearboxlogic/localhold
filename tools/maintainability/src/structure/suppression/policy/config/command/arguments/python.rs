mod bindings;
mod evaluation;
mod execution;
mod filesystem;
mod filesystem_write;
mod lexical;
mod process;

pub(super) use execution::{ArgvArgument, References as ExecutionReferences};
pub(super) use filesystem_write::{has_opaque_filesystem_write, has_opaque_filesystem_write_in_workspace};
use lexical::{executable_code, normalized_qualified_code};

const REJECTED_PYTHON_MODULES: &[&str] = &[
    "_interpreters",
    "_operator",
    "_pickle",
    "_sqlite3",
    "_testcapi",
    "_testinternalcapi",
    "_testlimitedcapi",
    "_tkinter",
    "_xxsubinterpreters",
    "code",
    "concurrent.interpreters",
    "cprofile",
    "dbm.sqlite3",
    "doctest",
    "gc",
    "idlelib",
    "inspect",
    "interpreters",
    "logging",
    "marshal",
    "multiprocessing",
    "operator",
    "optparse",
    "pdb",
    "pickle",
    "pkgutil",
    "profile",
    "pydoc",
    "shelve",
    "site",
    "sqlite3",
    "test",
    "timeit",
    "tkinter",
    "trace",
    "turtle",
    "turtledemo",
    "types",
    "unittest.mock",
    "webbrowser",
];

const UNCONDITIONAL_EXECUTION_MODULES: &[&str] = &[
    "_ctypes",
    "_frozen_importlib",
    "_frozen_importlib_external",
    "_imp",
    "_posixsubprocess",
    "_winapi",
    "bdb",
    "ensurepip",
    "pip",
];

pub(super) fn execution_references(path: &str, source: &str) -> ExecutionReferences {
    let normalized = normalize_continuations(source);
    let mut references = execution::collect(&normalized);
    references.opaque |= evaluation::has_non_ascii_code(&normalized);
    references.opaque |= references_unconditional_execution_capability(&normalized);
    references.opaque |= has_opaque_process_bindings(&normalized) && !bindings::is_reviewed_surface(path, source);
    references
}

pub(super) fn is_reviewed_dynamic_surface(path: &str, source: &str) -> bool {
    filesystem::is_reviewed_dynamic_write_surface(path, source)
}

pub(super) fn is_reviewed_process_surface(path: &str, source: &str) -> bool {
    process::is_reviewed_surface(path, source)
}

pub(super) fn normalize_continuations(source: &str) -> String {
    lexical::normalize_continuations(source)
}

pub(super) fn has_opaque_process_arguments(path: &str, source: &str) -> bool {
    let normalized = normalize_continuations(source);
    if evaluation::has_non_ascii_code(&normalized) {
        return true;
    }
    if references_unconditional_execution_capability(&normalized) {
        return true;
    }
    let reviewed_process_binding_surface = bindings::is_reviewed_surface(path, source);
    let reviewed_dynamic_surface = evaluation::is_reviewed_dynamic_code_surface(path, source);
    let reviewed_process_surface = process::is_reviewed_surface(path, source);
    if evaluation::has_dynamic_code(&normalized) && !reviewed_dynamic_surface {
        return true;
    }
    if references_command_capable_ffi(&normalized) {
        return true;
    }
    if has_opaque_process_bindings(&normalized) && !reviewed_process_binding_surface {
        return true;
    }
    if has_direct_dynamic_process_resolution(&normalized) && !reviewed_dynamic_surface {
        return true;
    }
    if has_dynamic_process_resolution(&normalized) && references_rust_tool(&normalized) {
        return true;
    }
    if references_exec_or_spawn_api(&normalized) && references_rust_tool(&normalized) {
        return true;
    }
    if references_process_api(&normalized) && process::has_non_literal_arguments(&normalized) && !reviewed_process_surface {
        return true;
    }
    normalized.lines().any(|line| {
        lexical::has_adjacent_literals(line) && (references_process_api(line) || references_rust_tool(line)) || references_process_api(line) && lexical::has_decoded_escape(line)
    })
}

pub(super) fn has_opaque_process_bindings(source: &str) -> bool {
    imports_command_capable_api(source)
        || uses_command_module_as_value(source)
        || uses_command_callable_as_value(source)
        || uses_dynamic_namespace_callable_as_value(source)
        || process::has_callable_reference(source)
}

pub(super) fn mutates_process_working_directory(source: &str) -> bool {
    let compact = executable_code(source).chars().filter(|character| !character.is_whitespace()).collect::<String>();
    ["os.chdir(", "os.fchdir(", "posix.chdir(", "posix.fchdir(", "contextlib.chdir("]
        .iter()
        .any(|call| compact.contains(call))
}

pub(super) fn mutates_process_environment(source: &str) -> bool {
    let executable = normalized_qualified_code(source);
    let compact = executable.chars().filter(|character| !character.is_whitespace()).collect::<String>();
    if [
        "os.putenv(",
        "os.unsetenv(",
        "posix.putenv(",
        "posix.unsetenv(",
        "os.environ.update(",
        "os.environ.clear(",
        "os.environ.pop(",
        "os.environ.popitem(",
        "os.environ.setdefault(",
        "os.environ.__setitem__(",
        "os.environ.__delitem__(",
    ]
    .iter()
    .any(|operation| compact.contains(operation))
    {
        return true;
    }
    executable.lines().any(environment_reference_is_opaque)
}

fn environment_reference_is_opaque(line: &str) -> bool {
    let mut remainder = line;
    while let Some((head, tail)) = remainder.split_once("os.environ") {
        let tail = tail.trim_start();
        if let Some(indexed) = tail.strip_prefix('[') {
            let Some(after_subscript) = after_matching_subscript(indexed) else {
                return true;
            };
            if has_delete_keyword(head) || starts_assignment(after_subscript.trim_start()) {
                return true;
            }
        } else if ![".get(", ".copy(", ".keys(", ".items(", ".values("].iter().any(|prefix| tail.starts_with(prefix)) {
            return true;
        }
        remainder = tail;
    }
    false
}

fn has_delete_keyword(head: &str) -> bool {
    let statement = head.rsplit(';').next().unwrap_or(head);
    statement.match_indices("del").any(|(index, keyword)| {
        let before = statement[..index].chars().next_back();
        let after = statement[index + keyword.len()..].chars().next();
        !before.is_some_and(is_identifier_character) && !after.is_some_and(is_identifier_character)
    })
}

fn after_matching_subscript(source: &str) -> Option<&str> {
    let mut expected = vec![']'];
    for (index, character) in source.char_indices() {
        if let Some(closing) = closing_delimiter(character) {
            expected.push(closing);
        } else if matches!(character, ')' | ']' | '}') {
            if expected.pop() != Some(character) {
                return None;
            }
            if expected.is_empty() {
                return Some(&source[index + character.len_utf8()..]);
            }
        }
    }
    None
}

fn starts_assignment(source: &str) -> bool {
    source.starts_with('=') && !source.starts_with("==")
        || ["+=", "-=", "*=", "/=", "//=", "%=", "**=", "&=", "|=", "^=", ">>=", "<<="]
            .iter()
            .any(|operator| source.starts_with(operator))
        || source.strip_prefix(':').is_some_and(annotation_has_assignment)
}

fn annotation_has_assignment(annotation: &str) -> bool {
    let mut expected = Vec::new();
    let characters = annotation.chars().collect::<Vec<_>>();
    for (index, character) in characters.iter().copied().enumerate() {
        if let Some(closing) = closing_delimiter(character) {
            expected.push(closing);
        } else if matches!(character, ')' | ']' | '}') {
            if expected.pop() != Some(character) {
                return true;
            }
        } else if character == '=' && expected.is_empty() {
            let before = index.checked_sub(1).and_then(|previous| characters.get(previous)).copied();
            let after = characters.get(index + 1).copied();
            if after != Some('=') && !matches!(before, Some('=' | '!' | '<' | '>' | ':')) {
                return true;
            }
        }
    }
    !expected.is_empty()
}

const fn closing_delimiter(character: char) -> Option<char> {
    match character {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        _ => None,
    }
}

fn imports_command_capable_api(source: &str) -> bool {
    executable_code(source).lines().flat_map(|line| line.split(';')).any(|statement| {
        let compact = statement.chars().filter(|character| !character.is_whitespace()).collect::<String>();
        let compact = compact.rsplit(':').next().unwrap_or(&compact);
        if compact
            .strip_prefix("import")
            .is_some_and(|imports| imports.split(',').any(|binding| rejected_module_binding(binding) || command_module_alias(binding)))
        {
            return true;
        }
        let Some((module, imports)) = compact.strip_prefix("from").and_then(|line| line.split_once("import")) else {
            return false;
        };
        if rejected_python_module(module) {
            return true;
        }
        let imports = imports.trim_matches(['(', ')']);
        imports.split(',').any(|binding| {
            let binding = binding.trim_matches(['(', ')']);
            let name = binding.split_once("as").map_or(binding, |(name, _)| name);
            if is_command_module(name) {
                return true;
            }
            match module {
                "asyncio" | "asyncio.subprocess" => name == "*" || matches!(name, "create_subprocess_exec" | "create_subprocess_shell"),
                "concurrent" => name == "interpreters",
                "dbm" => name == "sqlite3",
                "os" | "posix" => name == "*" || is_os_process_api(name),
                "subprocess" => name == "*" || is_subprocess_process_api(name),
                "pty" => matches!(name, "*" | "spawn"),
                "contextlib" => matches!(name, "*" | "chdir"),
                "logging" => name == "config",
                "sys" => matches!(name, "*" | "meta_path" | "modules" | "path_hooks" | "path_importer_cache"),
                "unittest" => name == "mock",
                _ => false,
            }
        })
    })
}

fn command_module_alias(binding: &str) -> bool {
    ["asyncio", "contextlib", "os", "posix", "pty", "subprocess", "sys"]
        .iter()
        .any(|module| binding.strip_prefix(module).is_some_and(|suffix| suffix.starts_with("as") && suffix.len() > 2))
}

fn rejected_module_binding(binding: &str) -> bool {
    REJECTED_PYTHON_MODULES.iter().any(|module| {
        binding == *module
            || binding
                .strip_prefix(module)
                .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with("as") && suffix.len() > 2)
    })
}

pub(super) fn rejected_python_module(name: &str) -> bool {
    REJECTED_PYTHON_MODULES
        .iter()
        .any(|module| name == *module || name.strip_prefix(module).is_some_and(|suffix| suffix.starts_with('.')))
}

fn uses_command_module_as_value(source: &str) -> bool {
    let executable = normalized_qualified_code(source);
    executable
        .lines()
        .flat_map(|line| line.split(';'))
        .filter(|statement| !starts_python_keyword(statement, "import") && !starts_python_keyword(statement, "from"))
        .any(|statement| {
            ["asyncio", "contextlib", "os", "posix", "pty", "subprocess", "sys"]
                .iter()
                .any(|module| standalone_module_value(statement, module) || nested_module_value(statement, module))
        })
}

fn uses_command_callable_as_value(source: &str) -> bool {
    let executable = normalized_qualified_code(source);
    executable
        .lines()
        .flat_map(|line| line.split(';'))
        .any(|statement| qualified_command_references(statement).any(|reference| reference.is_opaque()))
}

fn uses_dynamic_namespace_callable_as_value(source: &str) -> bool {
    let executable = normalized_qualified_code(source);
    executable.lines().flat_map(|line| line.split(';')).any(|statement| {
        ["getattr", "globals", "locals", "vars"].iter().any(|name| callable_name_is_value(statement, name)) || callable_name_is_value(statement, "operator.attrgetter")
    })
}

fn callable_name_is_value(statement: &str, name: &str) -> bool {
    statement.match_indices(name).any(|(index, _)| {
        let before = statement[..index].chars().next_back();
        let remainder = &statement[index + name.len()..];
        let after = remainder.chars().next();
        !before.is_some_and(is_identifier_character) && !after.is_some_and(is_identifier_character) && !remainder.trim_start().starts_with('(')
    })
}

struct QualifiedCommandReference<'a> {
    remainder: &'a str,
    kind: CommandReferenceKind,
}

impl QualifiedCommandReference<'_> {
    fn is_opaque(&self) -> bool {
        let remainder = self.remainder.trim_start();
        match self.kind {
            CommandReferenceKind::Callable => !remainder.starts_with('('),
            CommandReferenceKind::Environment => !remainder.starts_with('['),
        }
    }
}

#[derive(Clone, Copy)]
enum CommandReferenceKind {
    Callable,
    Environment,
}

fn qualified_command_references(statement: &str) -> impl Iterator<Item = QualifiedCommandReference<'_>> {
    statement.match_indices('.').filter_map(|(dot, _)| {
        let module_start = statement[..dot]
            .char_indices()
            .rev()
            .take_while(|(_, character)| is_identifier_character(*character))
            .last()
            .map_or(dot, |(index, _)| index);
        let module = &statement[module_start..dot];
        if module_start > 0 && statement[..module_start].ends_with('.') {
            return None;
        }
        let attribute_end = statement[dot + 1..]
            .char_indices()
            .take_while(|(_, character)| is_identifier_character(*character) || *character == '.')
            .last()
            .map_or(dot + 1, |(index, character)| dot + 1 + index + character.len_utf8());
        let attribute = &statement[dot + 1..attribute_end];
        let (kind, consumed) = command_attribute_prefix(module, attribute)?;
        Some(QualifiedCommandReference {
            remainder: &statement[dot + 1 + consumed..],
            kind,
        })
    })
}

fn command_attribute_prefix(module: &str, attribute: &str) -> Option<(CommandReferenceKind, usize)> {
    let first = attribute.split('.').next().unwrap_or_default();
    match module {
        "os" | "posix" => {
            if first == "__getattribute__" {
                return Some((CommandReferenceKind::Callable, first.len()));
            }
            if matches!(first, "chdir" | "fchdir" | "popen" | "putenv" | "startfile" | "system" | "unsetenv") || first.starts_with("exec") || first.starts_with("spawn") {
                return Some((CommandReferenceKind::Callable, first.len()));
            }
            if let Some(method) = attribute.strip_prefix("environ.").and_then(|suffix| suffix.split('.').next()) {
                if matches!(method, "update" | "clear" | "pop" | "popitem" | "setdefault" | "__setitem__" | "__delitem__") {
                    return Some((CommandReferenceKind::Callable, "environ.".len() + method.len()));
                }
                if matches!(method, "get" | "copy" | "keys" | "items" | "values") {
                    return None;
                }
            }
            (first == "environ").then_some((CommandReferenceKind::Environment, first.len()))
        }
        "subprocess" if is_subprocess_process_api(first) || first == "__getattribute__" => Some((CommandReferenceKind::Callable, first.len())),
        "asyncio" if matches!(first, "create_subprocess_exec" | "create_subprocess_shell" | "__getattribute__") => Some((CommandReferenceKind::Callable, first.len())),
        "pty" if matches!(first, "spawn" | "__getattribute__") => Some((CommandReferenceKind::Callable, first.len())),
        "contextlib" if matches!(first, "chdir" | "__getattribute__") => Some((CommandReferenceKind::Callable, first.len())),
        "sys" if first == "__getattribute__" => Some((CommandReferenceKind::Callable, first.len())),
        _ => None,
    }
}

fn starts_python_keyword(statement: &str, keyword: &str) -> bool {
    strip_python_keyword(statement, keyword).is_some()
}

fn strip_python_keyword<'a>(statement: &'a str, keyword: &str) -> Option<&'a str> {
    statement.trim_start().strip_prefix(keyword).filter(|tail| tail.starts_with(char::is_whitespace))
}

fn standalone_module_value(statement: &str, module: &str) -> bool {
    statement.match_indices(module).any(|(index, _)| {
        let before = statement[..index].chars().next_back();
        let after = statement[index + module.len()..].chars().next();
        if before == Some('.') || before.is_some_and(is_identifier_character) || after.is_some_and(is_identifier_character) {
            return false;
        }
        !statement[index + module.len()..].trim_start().starts_with('.')
    })
}

fn nested_module_value(statement: &str, module: &str) -> bool {
    let reference = format!(".{module}");
    statement.match_indices(&reference).any(|(index, _)| {
        let end = index + reference.len();
        statement[..index].chars().next_back().is_some_and(is_identifier_character) && !statement[end..].chars().next().is_some_and(is_identifier_character)
    })
}

fn is_command_module(name: &str) -> bool {
    matches!(name, "asyncio" | "contextlib" | "os" | "posix" | "pty" | "subprocess" | "sys")
}

fn is_os_process_api(name: &str) -> bool {
    matches!(
        name,
        "chdir" | "environ" | "fchdir" | "popen" | "posix_spawn" | "posix_spawnp" | "putenv" | "startfile" | "system" | "unsetenv"
    ) || name.starts_with("exec")
        || name.starts_with("spawn")
}

fn is_subprocess_process_api(name: &str) -> bool {
    matches!(
        name,
        "_fork_exec" | "call" | "check_call" | "check_output" | "getoutput" | "getstatusoutput" | "popen" | "run"
    )
}

fn references_command_capable_ffi(source: &str) -> bool {
    let compact = lexical::without_literals(source)
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "importctypes",
        "fromctypesimport",
        "importcffi",
        "fromcffiimport",
        "cdll(",
        "pydll(",
        "windll(",
        "oledll(",
        "cfunctype(",
        "pyfunctype(",
        "winfunctype(",
        ".dlopen(",
    ]
    .iter()
    .any(|name| compact.contains(name))
}

fn references_unconditional_execution_capability(source: &str) -> bool {
    let executable = normalized_qualified_code(source);
    imports_unconditional_execution_module(&executable)
        || executable
            .match_indices(".createprocess")
            .any(|(index, name)| identifier_is_exact_at(&executable, index + 1, name.len() - 1))
        || ["enable_load_extension", "load_extension", "sqlite_dbconfig_enable_load_extension"]
            .iter()
            .any(|name| executable.match_indices(name).any(|(index, _)| identifier_is_exact_at(&executable, index, name.len())))
}

fn imports_unconditional_execution_module(source: &str) -> bool {
    source.lines().flat_map(|line| line.split(';')).any(|statement| {
        let statement = statement.rsplit(':').next().unwrap_or(statement);
        if let Some(imports) = strip_python_keyword(statement, "import") {
            return imports.split(',').any(unconditional_execution_module_binding);
        }
        let Some(from) = strip_python_keyword(statement, "from") else {
            return false;
        };
        let from = from.trim_start();
        let Some(module) = from.split_whitespace().next() else {
            return false;
        };
        unconditional_execution_module(module) && from.strip_prefix(module).and_then(|tail| strip_python_keyword(tail, "import")).is_some()
    })
}

fn unconditional_execution_module_binding(binding: &str) -> bool {
    binding.split_whitespace().next().is_some_and(unconditional_execution_module)
}

fn unconditional_execution_module(module: &str) -> bool {
    UNCONDITIONAL_EXECUTION_MODULES
        .iter()
        .any(|candidate| module == *candidate || module.strip_prefix(candidate).is_some_and(|suffix| suffix.starts_with('.')))
}

fn identifier_is_exact_at(source: &str, index: usize, length: usize) -> bool {
    !source[..index].chars().next_back().is_some_and(is_identifier_character) && !source[index + length..].chars().next().is_some_and(is_identifier_character)
}

fn references_process_api(source: &str) -> bool {
    let source = normalized_qualified_code(source).to_ascii_lowercase();
    [
        "asyncio.create_subprocess_",
        "os.system",
        "os.popen",
        "os.startfile",
        "posix.system",
        "posix.popen",
        "pty.spawn",
    ]
    .iter()
    .any(|name| source.contains(name))
        || references_root_module(&source, "subprocess")
        || references_exec_or_spawn_api(&source)
}

fn references_root_module(source: &str, module: &str) -> bool {
    source.match_indices(module).any(|(index, _)| {
        let before = source[..index].chars().next_back();
        let after = source[index + module.len()..].chars().next();
        before != Some('.') && !before.is_some_and(is_identifier_character) && after == Some('.')
    })
}

fn references_exec_or_spawn_api(source: &str) -> bool {
    let source = normalized_qualified_code(source).to_ascii_lowercase();
    ["execl", "execv", "spawn"].iter().any(|name| source.contains(name))
}

fn has_dynamic_process_resolution(source: &str) -> bool {
    let compact = lexical::without_literals(source)
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "__import__(",
        "getattr(",
        "globals(",
        "importlib.",
        "locals(",
        "operator.attrgetter(",
        "operator.methodcaller(",
        "sys.modules",
        "vars(",
    ]
    .iter()
    .any(|name| compact.contains(name))
}

fn has_direct_dynamic_process_resolution(source: &str) -> bool {
    let compact = lexical::without_literals(source)
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if compact.contains("sys.modules")
        || [
            ".__dict__",
            ".__getattribute__(",
            "getattr(",
            "globals(",
            "locals(",
            "operator.attrgetter(",
            "operator.methodcaller(",
            "vars(",
        ]
        .iter()
        .any(|access| compact.contains(access))
    {
        return true;
    }
    ["asyncio", "contextlib", "os", "posix", "pty", "subprocess", "sys"].iter().any(|module| {
        compact.contains(&format!("{module}.__dict__"))
            || compact.contains(&format!("{module}.__getattribute__("))
            || compact.contains(&format!("getattr({module},"))
            || compact.contains(&format!("vars({module})"))
    })
}

fn references_rust_tool(source: &str) -> bool {
    let source = source.to_ascii_lowercase();
    if ["cargo", "rustc", "rustdoc", "clippy-driver"].iter().any(|name| source.contains(name)) {
        return true;
    }
    let compact = source.chars().filter(|character| character.is_alphanumeric()).collect::<String>();
    ["cargo", "rustc", "rustdoc", "clippydriver"].iter().any(|name| compact.contains(name))
}

fn is_identifier_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}
#[cfg(test)]
mod tests;
