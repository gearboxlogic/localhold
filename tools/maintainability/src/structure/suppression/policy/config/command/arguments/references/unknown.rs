use std::path::Path;

use super::ValueSemantics;

pub(super) fn reviewed_dynamic_program(surface: &str, command: &str, reviewed_git_wrappers: bool) -> Option<&'static str> {
    (reviewed_git_wrappers && surface == "script/check-maintainability-bootstrap.sh" && command == "git_command").then_some("git")
}

pub(super) fn is_preclassified_command(surface: &str, command: &str) -> bool {
    let command = command.strip_suffix(".exe").unwrap_or(command);
    is_shell_builtin(command)
        || is_standard_utility(command)
        || matches!(command, "cargo" | "cygpath" | "just" | "openssl" | "out-null" | "pkg-config")
        || surface == "script/test-postgres-smoke.sh" && matches!(command, "$container_cli" | "container_cli")
}

fn is_shell_builtin(command: &str) -> bool {
    matches!(command, "[" | "[[" | ":" | "case" | "done" | "else" | "esac" | "fi" | "for" | "in")
        || matches!(command, "break" | "continue" | "exit" | "false" | "return" | "shift" | "true" | "wait")
        || matches!(command, "cd" | "export" | "local" | "read" | "readonly" | "set" | "umask" | "unset")
        || matches!(command, "compgen" | "echo" | "printf" | "pwd" | "test" | "type")
}

fn is_standard_utility(command: &str) -> bool {
    matches!(command, "basename" | "cat" | "chmod" | "cmp" | "cp" | "copy-item" | "diff")
        || matches!(command, "dirname" | "head" | "install" | "ln" | "mkdir" | "mktemp" | "mv")
        || matches!(command, "readlink" | "realpath" | "rm" | "rmdir" | "split" | "tail" | "tee" | "touch" | "unzip")
        || matches!(command, "grep" | "rg" | "ripgrep" | "seq" | "sort" | "wc")
        || matches!(command, "cc" | "clang" | "gcc" | "gitleaks" | "rustc" | "rustup")
        || matches!(command, "sha256sum" | "shasum" | "tar" | "uname" | "zip")
        || matches!(command, "kill" | "mapfile" | "readarray" | "sleep")
}

#[cfg(test)]
pub(super) fn execution_inputs<'a>(_surface: &str, command: &str, arguments: &'a [String]) -> (Vec<&'a str>, bool) {
    execution_inputs_with_semantics(command, arguments, ValueSemantics::Shell)
}

pub(super) fn execution_inputs_with_semantics<'a>(command: &str, arguments: &'a [String], semantics: ValueSemantics) -> (Vec<&'a str>, bool) {
    let rust_sources = arguments.iter().map(String::as_str).filter(|argument| is_rust_source(argument)).collect::<Vec<_>>();
    let literal_command = !contains_dynamic(semantics, command);
    let opaque = literal_command
        || !rust_sources.is_empty()
        || arguments.iter().any(|argument| {
            argument.to_ascii_lowercase().contains(".rs")
                || matches!(argument.as_str(), "-c" | "--command" | "--eval" | "--execute")
                || ["--command=", "--eval=", "--execute="].iter().any(|prefix| argument.starts_with(prefix))
        })
        || arguments
            .iter()
            .any(|argument| !matches!(argument.as_str(), "(" | ")" | "{" | "}") && contains_dynamic(semantics, argument));
    (rust_sources, opaque)
}

pub(super) fn gitleaks_policy_inputs(arguments: &[String], semantics: ValueSemantics) -> (Vec<&str>, bool) {
    let inputs = ["--config", "--gitleaks-ignore-path"]
        .into_iter()
        .filter_map(|option| arguments.windows(2).find(|pair| pair[0] == option).map(|pair| pair[1].as_str()))
        .collect::<Vec<_>>();
    let unresolved = inputs.len() != 2 || inputs.iter().any(|input| input.starts_with('-') || semantics.contains_dynamic(input));
    (inputs, unresolved)
}

pub(super) fn gitleaks_data_is_opaque(path: &str, source: &str) -> Option<bool> {
    (path == ".github/gitleaks.toml")
        .then(|| {
            source.parse::<toml::Table>().map_or(true, |config| {
                config
                    .get("extend")
                    .is_some_and(|extend| extend.as_table().is_none_or(|extend| extend.contains_key("path")))
            })
        })
        .or_else(|| (path == ".github/gitleaksignore").then_some(false))
}

pub(super) fn is_execution_input_prefix(word: &str) -> bool {
    matches!(word, "!" | "if" | "then" | "elif" | "while" | "until" | "do") || word.starts_with('-')
}

pub(super) fn is_rust_source(argument: &str) -> bool {
    Path::new(argument)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

fn contains_dynamic_value(value: &str) -> bool {
    value.contains('`') || value.contains(['*', '?', '[', '{', '~', '%', '!']) || value.as_bytes().windows(2).any(|pair| pair[0] == b'$' && pair[1] != b'\'')
}

fn contains_dynamic(semantics: ValueSemantics, value: &str) -> bool {
    semantics == ValueSemantics::Shell && contains_dynamic_value(value)
}

#[cfg(test)]
mod tests {
    use super::{execution_inputs, gitleaks_policy_inputs, is_preclassified_command};

    fn inputs(command: &str) -> (Vec<String>, bool) {
        let (candidates, opaque) = super::super::collect_execution_inputs(std::iter::once(command), true, "script/check.sh", false);
        (candidates.into_iter().collect(), opaque)
    }

    #[test]
    fn unknown_program_inputs_fail_closed_for_rust_paths_and_inline_code() {
        for arguments in [
            vec!["quality/runner.rs".to_owned()],
            vec!["quality/runner.RS".to_owned()],
            vec!["./quality/runner.rs".to_owned()],
        ] {
            assert_eq!(execution_inputs("script/check.sh", "unknown-interpreter", &arguments), (vec![arguments[0].as_str()], true));
        }
        for arguments in [
            vec!["quality/*.rs".to_owned()],
            vec!["--program=quality/runner.rs".to_owned()],
            vec!["-c".to_owned(), ". quality/runner.rs; :".to_owned()],
            vec!["quality/runner.$suffix".to_owned()],
        ] {
            assert!(execution_inputs("script/check.sh", "unknown-interpreter", &arguments).1, "{arguments:?}");
        }
        assert!(execution_inputs("script/check.sh", "$reviewed_program", &["$reviewed_operand".to_owned()]).1);
    }

    #[test]
    fn gitleaks_configuration_is_an_execution_input() {
        let arguments = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect::<Vec<_>>();
        let separate = arguments(&["git", "--config", ".github/gitleaks.toml", "--gitleaks-ignore-path", ".github/gitleaksignore"]);
        assert_eq!(
            gitleaks_policy_inputs(&separate, super::ValueSemantics::Shell),
            (vec![".github/gitleaks.toml", ".github/gitleaksignore"], false)
        );
        assert!(
            gitleaks_policy_inputs(
                &arguments(&["git", "--config", "$CONFIG", "--gitleaks-ignore-path", ".ignore"]),
                super::ValueSemantics::Shell
            )
            .1
        );
        assert!(gitleaks_policy_inputs(&arguments(&["git", "--config"]), super::ValueSemantics::Shell).1);
    }

    #[test]
    fn data_consumers_are_explicit_and_surface_specific() {
        for command in ["wc", "head", "diff", "rg", "printf", "tee", "[["] {
            assert!(is_preclassified_command("script/check.sh", command), "{command}");
        }
        assert!(is_preclassified_command("script/test-postgres-smoke.sh", "container_cli"));
        assert!(!is_preclassified_command("script/check.sh", "container_cli"));
        assert!(!is_preclassified_command("script/check.sh", "has_write_mode_bits"));
        assert!(!is_preclassified_command("script/check.sh", "unknown-interpreter"));
    }

    #[test]
    fn alternate_interpreters_cannot_hide_rust_execution_inputs() {
        for command in ["ash quality/runner.rs", "suffix=rs; ash quality/runner.$suffix", "ash -c '. quality/runner.rs; :'"] {
            assert_eq!(inputs(command), (Vec::new(), true), "{command}");
        }
        assert_eq!(inputs("unknown-interpreter quality/runner.rs"), (vec!["quality/runner.rs".to_owned()], true));
        assert_eq!(inputs("suffix=rs; unknown-interpreter quality/runner.$suffix"), (Vec::new(), true));
        assert_eq!(inputs("set -- ash quality/runner.rs; \"$@\""), (Vec::new(), true));
        assert_eq!(inputs("args=(ash quality/runner.rs); \"${args[@]}\""), (Vec::new(), true));
        for command in ["wc -l src/lib.rs", "head src/lib.rs", "diff before.rs after.rs", "rg pattern src/lib.rs"] {
            assert_eq!(inputs(command), (Vec::new(), false), "{command}");
        }
    }

    #[test]
    fn declared_function_calls_do_not_hide_opaque_function_bodies() {
        assert_eq!(inputs("safe() {\n  printf '%s\\n' \"$1\"\n}\nsafe value"), (Vec::new(), false));
        assert_eq!(inputs("unsafe_runner() {\n  ash quality/runner.rs\n}\nunsafe_runner value"), (Vec::new(), true));
        assert_eq!(inputs("dispatcher() {\n  \"$@\"\n}\ndispatcher ash quality/runner.rs"), (Vec::new(), true));
        assert_eq!(inputs("runner() { ash quality/runner.rs; }\nrunner"), (Vec::new(), true));
        assert_eq!(inputs("function runner { ash quality/runner.rs; }\nrunner"), (Vec::new(), true));
        assert_eq!(inputs("safe() { printf '%s\\n' value; }\nsafe"), (Vec::new(), true));
    }
}
