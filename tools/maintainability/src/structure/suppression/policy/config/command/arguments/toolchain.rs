use super::{is_environment_assignment, is_rust_tool_token, is_shell_command_prefix, tool_basename};

const REVIEWED_SELECTORS: [&str; 2] = ["+1.97.0", "+nightly"];

pub(super) fn uses_unreviewed_selector(tokens: &[String], case_insensitive_tools: bool) -> bool {
    let Some(tool_index) = tokens.iter().position(|token| is_rust_tool_token(token, case_insensitive_tools)) else {
        return false;
    };
    tokens
        .get(tool_index + 1)
        .filter(|argument| argument.starts_with('+'))
        .is_some_and(|selector| !REVIEWED_SELECTORS.contains(&selector.as_str()))
}

pub(super) fn registers_custom_toolchain(tokens: &[String], case_insensitive_tools: bool) -> bool {
    let Some(command_index) = executable_index(tokens) else {
        return false;
    };
    let command = tool_basename(&tokens[command_index]);
    let is_rustup = command == "rustup" || command == "rustup.exe" || case_insensitive_tools && matches!(command.to_ascii_lowercase().as_str(), "rustup" | "rustup.exe");
    is_rustup
        && tokens[command_index + 1..]
            .windows(2)
            .any(|arguments| arguments[0].eq_ignore_ascii_case("toolchain") && arguments[1].eq_ignore_ascii_case("link"))
}

fn executable_index(tokens: &[String]) -> Option<usize> {
    tokens.iter().position(|token| {
        let word = token.trim_matches(['(', ')', '{', '}']);
        !word.is_empty() && !is_shell_command_prefix(word) && !is_environment_assignment(word)
    })
}

#[cfg(test)]
mod tests {
    use super::{registers_custom_toolchain, uses_unreviewed_selector};

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
        assert!(registers_custom_toolchain(&tokens(&["rustup", "toolchain", "link", "fake", "/tmp/toolchain"]), true));
        assert!(registers_custom_toolchain(
            &tokens(&["command", "RUSTUP.EXE", "-v", "toolchain", "link", "fake", "C:\\toolchain"]),
            true
        ));
        assert!(!registers_custom_toolchain(&tokens(&["rustup", "toolchain", "install", "1.97.0"]), true));
        assert!(!registers_custom_toolchain(&tokens(&["echo", "rustup", "toolchain", "link"]), true));
    }
}
