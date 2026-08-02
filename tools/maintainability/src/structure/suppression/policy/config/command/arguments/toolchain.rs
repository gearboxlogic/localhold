use super::{is_environment_assignment, is_rust_tool_token, is_shell_command_prefix, tool_basename};

const PINNED_TOOLCHAIN: &str = "1.97.0";
const REVIEWED_TOOLCHAINS: [&str; 2] = [PINNED_TOOLCHAIN, "nightly"];

pub(super) fn uses_unreviewed_selector(tokens: &[String], case_insensitive_tools: bool) -> bool {
    let Some(tool_index) = tokens.iter().position(|token| is_rust_tool_token(token, case_insensitive_tools)) else {
        return false;
    };
    tokens
        .get(tool_index + 1)
        .filter(|argument| argument.starts_with('+'))
        .is_some_and(|selector| !is_reviewed_toolchain(selector))
}

pub(super) fn uses_unreviewed_rustup_configuration(tokens: &[String], case_insensitive_tools: bool) -> bool {
    let Some(command_index) = executable_index(tokens) else {
        return false;
    };
    let command = tool_basename(&tokens[command_index]);
    let is_rustup = command == "rustup" || command == "rustup.exe" || case_insensitive_tools && matches!(command.to_ascii_lowercase().as_str(), "rustup" | "rustup.exe");
    if !is_rustup {
        return false;
    }
    let arguments = &tokens[command_index + 1..];
    arguments
        .windows(2)
        .any(|arguments| arguments[0].eq_ignore_ascii_case("toolchain") && arguments[1].eq_ignore_ascii_case("link"))
        || arguments
            .windows(2)
            .any(|arguments| arguments[0].eq_ignore_ascii_case("default") && !is_reviewed_toolchain(&arguments[1]))
        || arguments
            .windows(3)
            .any(|arguments| arguments[0].eq_ignore_ascii_case("override") && arguments[1].eq_ignore_ascii_case("set") && !is_reviewed_toolchain(&arguments[2]))
}

fn is_reviewed_toolchain(toolchain: &str) -> bool {
    REVIEWED_TOOLCHAINS.contains(&toolchain.trim_start_matches('+'))
}

fn executable_index(tokens: &[String]) -> Option<usize> {
    tokens.iter().position(|token| {
        let word = token.trim_matches(['(', ')', '{', '}']);
        !word.is_empty() && !is_shell_command_prefix(word) && !is_environment_assignment(word)
    })
}

#[cfg(test)]
mod tests {
    use super::{uses_unreviewed_rustup_configuration, uses_unreviewed_selector};

    fn tokens(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn only_reviewed_cargo_toolchain_selectors_are_allowed() {
        assert!(uses_unreviewed_selector(&tokens(&["cargo", "+fake", "clippy"]), true));
        assert!(uses_unreviewed_selector(&tokens(&["cargo.exe", "+stable", "test"]), true));
        assert!(!uses_unreviewed_selector(&tokens(&["cargo", "+1.97.0", "clippy"]), true));
        assert!(!uses_unreviewed_selector(&tokens(&["cargo", "+nightly", "fmt"]), true));
        assert!(!uses_unreviewed_selector(&tokens(&["cargo", "test"]), true));
    }

    #[test]
    fn rustup_toolchain_link_is_a_custom_registration() {
        assert!(uses_unreviewed_rustup_configuration(
            &tokens(&["rustup", "toolchain", "link", "fake", "/tmp/toolchain"]),
            true
        ));
        assert!(uses_unreviewed_rustup_configuration(
            &tokens(&["command", "RUSTUP.EXE", "-v", "toolchain", "link", "fake", "C:\\toolchain"]),
            true
        ));
        assert!(!uses_unreviewed_rustup_configuration(&tokens(&["rustup", "toolchain", "install", "1.97.0"]), true));
        assert!(!uses_unreviewed_rustup_configuration(&tokens(&["echo", "rustup", "toolchain", "link"]), true));
    }

    #[test]
    fn persistent_rustup_selection_requires_a_reviewed_toolchain() {
        assert!(uses_unreviewed_rustup_configuration(&tokens(&["rustup", "default", "stable"]), true));
        assert!(uses_unreviewed_rustup_configuration(&tokens(&["rustup", "override", "set", "fake"]), true));
        assert!(!uses_unreviewed_rustup_configuration(&tokens(&["rustup", "default", "1.97.0"]), true));
        assert!(!uses_unreviewed_rustup_configuration(&tokens(&["rustup", "override", "set", "nightly"]), true));
    }
}
