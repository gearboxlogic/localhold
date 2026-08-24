use super::super::{has_executable_unquoted_heredoc, has_opaque_heredoc_delimiter, without_noncommand_shell_data};

#[test]
fn payloads_are_not_parsed_as_shell_commands() {
    let source = "cat <<'DOC'\n  PREFIX/bin/hold\nDOC\ncat <<-SCRIPT\n\t./generated-command\n\tSCRIPT\nquality/run-lints\n";
    let normalized = without_noncommand_shell_data(source);
    assert!(!normalized.contains("PREFIX/bin/hold"));
    assert!(!normalized.contains("./generated-command"));
    assert!(normalized.contains("quality/run-lints"));
}

#[test]
fn empty_quoted_delimiters_preserve_following_commands() {
    let source = "cat <<'' >/dev/null\n\nquality/run-lints\n";
    let normalized = without_noncommand_shell_data(source);
    assert!(normalized.contains("quality/run-lints"));
    assert!(!has_opaque_heredoc_delimiter(source));
}

#[test]
fn context_dependent_delimiters_fail_closed() {
    for source in [
        "cat <<$'EOF' >/dev/null\nEOF\nquality/run-lints\n",
        "cat <<$'E\\x4fF' >/dev/null\nEOF\nquality/run-lints\n",
        "cat <<$\"EOF\" >/dev/null\nEOF\nquality/run-lints\n",
    ] {
        assert!(has_opaque_heredoc_delimiter(source), "{source}");
        assert!(without_noncommand_shell_data(source).contains("quality/run-lints"), "{source}");
    }
}

#[test]
fn double_quoted_delimiters_preserve_literal_backslashes() {
    let source = "cat <<\"D\\qOC\" >/dev/null\nignored\nD\\qOC\nquality/run-lints\n";
    let normalized = without_noncommand_shell_data(source);
    assert!(normalized.contains("quality/run-lints"));
    assert!(!has_opaque_heredoc_delimiter(source));

    for source in ["cat <<$'EOF\nquality/run-lints\n", "cat <<\\\nquality/run-lints\n"] {
        assert!(has_executable_unquoted_heredoc(source), "{source}");
    }
}
