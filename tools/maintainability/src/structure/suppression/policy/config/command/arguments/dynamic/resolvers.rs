use super::super::tokens;

const TRUSTED_SYSTEM_COMMAND: &str = r#"trusted_system_command() {
    local name=$1
    local candidate
    candidate=$(type -P -- "$name") || {
        printf 'maintainability bootstrap requires command: %s\n' "$name" >&2
        exit 1
    }
    local directory=${candidate%/*}
    [[ $directory != "$candidate" ]] || directory=.
    directory=$(cd -- "$directory" && pwd -P)
    candidate="$directory/${candidate##*/}"
    case "$directory" in
        /bin | /usr/bin | /mingw64/bin) ;;
        *)
            printf 'maintainability bootstrap requires an OS-owned command: %s\n' "$candidate" >&2
            exit 1
            ;;
    esac
    if [[ ! -f "$candidate" || ! -x "$candidate" ]]; then
        printf 'maintainability bootstrap requires an executable system file: %s\n' "$candidate" >&2
        exit 1
    fi
    printf '%s\n' "$candidate"
}"#;

const GATE_SHA256_FILE: &str = r#"sha256_file() {
    local output
    output=$("$sha256_command" -- "$1")
    printf '%s\n' "${output%%[[:space:]]*}"
}"#;

const AUTHENTICATED_TOOL: &str = r#"authenticated_tool() {
    local path=$1
    local expected=$2
    if [[ ! -f $path || -L $path || ! -x $path ]]; then
        printf 'pinned Rust tool must be a regular non-symlink executable: %s\n' "$path" >&2
        exit 1
    fi
    local actual
    actual=$(sha256_file "$path")
    if [[ $actual != "$expected" ]]; then
        printf 'reviewed Rust tool digest differs: %s\n' "$path" >&2
        exit 1
    fi
    printf '%s\n' "$path"
}"#;

pub(super) struct Candidate<'a> {
    pub(super) path: &'a str,
    pub(super) name: &'a str,
    pub(super) resolver: &'a str,
    pub(super) resolver_command: &'a [String],
    pub(super) assignment_command: &'a [String],
    pub(super) source: &'a str,
    pub(super) source_is_reviewed: bool,
}

pub(super) fn is_reviewed(candidate: &Candidate<'_>) -> bool {
    if !candidate.source_is_reviewed {
        return false;
    }
    if candidate.resolver == "trusted_system_command" && candidate.path == "script/check-maintainability-bootstrap.sh" {
        let expected_tool = candidate
            .name
            .strip_suffix("_command")
            .map_or(candidate.name, |tool| if tool == "sha256" { "sha256sum" } else { tool });
        return matches!(
            candidate.assignment_command,
            [assignment, tool]
                if assignment == &format!("{}=$(trusted_system_command", candidate.name)
                    && tool == &format!("{expected_tool})")
                    && exact_function(candidate.source, "trusted_system_command", TRUSTED_SYSTEM_COMMAND)
        );
    }
    candidate.path == "script/run-maintainability-gate.sh"
        && match candidate.resolver {
            "authenticated_tool" => {
                reviewed_authenticated_tool(candidate.name, candidate.resolver_command)
                    && exact_function(candidate.source, "authenticated_tool", AUTHENTICATED_TOOL)
                    && exact_function(candidate.source, "sha256_file", GATE_SHA256_FILE)
            }
            "sha256_file" => exact_function(candidate.source, "sha256_file", GATE_SHA256_FILE),
            "$cygpath_command" => candidate.name == "rustup_executable" && candidate.resolver_command == ["$cygpath_command", "-u", "$rustup_executable"],
            _ => false,
        }
}

fn reviewed_authenticated_tool(name: &str, command: &[String]) -> bool {
    matches!(
        (name, command),
        ("rustup_executable", [resolver, path, digest])
            if resolver == "authenticated_tool" && path == "$rustup_executable" && digest == "$expected_rustup_sha256"
    ) || matches!(
        (name, command),
        ("cargo_executable", [resolver, path, digest])
            if resolver == "authenticated_tool" && path == "$toolchain_bin/cargo$tool_extension" && digest == "$expected_cargo_sha256"
    ) || matches!(
        (name, command),
        ("cargo_clippy_executable", [resolver, path, digest])
            if resolver == "authenticated_tool"
                && path == "$toolchain_bin/cargo-clippy$tool_extension"
                && digest == "$expected_cargo_clippy_sha256"
    ) || matches!(
        (name, command),
        ("cargo_fmt_executable", [resolver, path, digest])
            if resolver == "authenticated_tool" && path == "$toolchain_bin/cargo-fmt$tool_extension" && digest == "$expected_cargo_fmt_sha256"
    )
}

pub(super) fn has_trusted_system_command(source: &str) -> bool {
    exact_function(source, "trusted_system_command", TRUSTED_SYSTEM_COMMAND)
}

fn exact_function(source: &str, name: &str, definition: &str) -> bool {
    let command_source = tokens::without_noncommand_shell_data(source);
    let mut definitions = source.match_indices(definition);
    let Some((offset, _)) = definitions.next() else {
        return false;
    };
    if definitions.next().is_some() {
        return false;
    }
    let line_index = source[..offset].bytes().filter(|byte| *byte == b'\n').count();
    let Some(declaration) = definition.lines().next() else {
        return false;
    };
    command_source.lines().nth(line_index).is_some_and(|line| line.trim_start() == declaration) && tokens::declared_shell_function_count(&command_source, name) == 1
}

#[cfg(test)]
mod tests {
    use super::{AUTHENTICATED_TOOL, GATE_SHA256_FILE, TRUSTED_SYSTEM_COMMAND, exact_function};

    #[test]
    fn resolver_functions_are_whole_definition_profiles() {
        assert!(exact_function(TRUSTED_SYSTEM_COMMAND, "trusted_system_command", TRUSTED_SYSTEM_COMMAND));
        assert!(!exact_function(
            &TRUSTED_SYSTEM_COMMAND.replace("type -P", "printf /tmp/helper"),
            "trusted_system_command",
            TRUSTED_SYSTEM_COMMAND
        ));
        let gate = format!("{GATE_SHA256_FILE}\n\n{AUTHENTICATED_TOOL}\n");
        assert!(exact_function(&gate, "sha256_file", GATE_SHA256_FILE));
        assert!(exact_function(&gate, "authenticated_tool", AUTHENTICATED_TOOL));
        assert!(!exact_function(
            &format!("{gate}\nfunction authenticated_tool\n{{\n  printf /tmp/helper\n}}\n"),
            "authenticated_tool",
            AUTHENTICATED_TOOL
        ));
        assert!(!exact_function(
            &format!("cat <<'PROFILE'\n{AUTHENTICATED_TOOL}\nPROFILE\nfunction authenticated_tool\n{{\n  printf /tmp/helper\n}}\n"),
            "authenticated_tool",
            AUTHENTICATED_TOOL
        ));
    }
}
