pub(super) fn name(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    super::super::super::is_rust_tool_token(&normalized, true)
        || normalized == "just"
        || super::super::super::references::wrapper::is_command_launcher(&normalized)
        || super::super::super::references::shell::is_additional(&normalized)
        || super::super::super::dynamic_program::is_unanalyzed_interpreter(&normalized)
        || super::super::super::dynamic_program::is_python_interpreter(&normalized)
        || MODELED_COMMANDS.contains(&normalized.as_str())
}

const MODELED_COMMANDS: &[&str] = &[
    "alias",
    ".",
    "bash",
    "break",
    "builtin",
    "cd",
    "command",
    "continue",
    "coproc",
    "dash",
    "declare",
    "enable",
    "eval",
    "exec",
    "exit",
    "export",
    "fc",
    "fish",
    "getopts",
    "hash",
    "local",
    "let",
    "mapfile",
    "printf",
    "powershell",
    "pwsh",
    "read",
    "readarray",
    "readonly",
    "return",
    "set",
    "sh",
    "source",
    "test",
    "trap",
    "typeset",
    "unset",
    "wait",
    "xargs",
    "zsh",
];
