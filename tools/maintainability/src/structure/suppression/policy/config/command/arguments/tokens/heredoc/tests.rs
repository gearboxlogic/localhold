use super::super::{has_executable_unquoted_heredoc, has_opaque_heredoc_delimiter, shell_expansion_source, without_noncommand_shell_data};

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
fn continued_commands_collect_every_heredoc_before_consuming_bodies() {
    let opaque = "cat <<'ONE' \\\n  <<$'T\\x57O' >/dev/null\none\nONE\ntwo\nTWO\n";
    assert!(has_opaque_heredoc_delimiter(opaque));

    let static_delimiters = "cat <<'ONE' \\\n  <<TWO >/dev/null\none\nONE\n$(generated-command)\nTWO\nquality/run-lints\n";
    let commands = without_noncommand_shell_data(static_delimiters);
    assert!(!commands.contains("generated-command"));
    assert!(commands.contains("quality/run-lints"));
    assert!(has_executable_unquoted_heredoc(static_delimiters));
    assert!(shell_expansion_source(static_delimiters).contains("$(generated-command)"));

    let quoted = static_delimiters.replace("<<TWO", "<<'TWO'");
    assert!(!has_opaque_heredoc_delimiter(&quoted));
    assert!(!has_executable_unquoted_heredoc(&quoted));

    let pipe_standard_error = opaque.replace("\\\n  <<", "|&\n  <<");
    assert!(has_opaque_heredoc_delimiter(&pipe_standard_error));
}

#[test]
fn continuation_formed_heredoc_openers_fail_closed() {
    for source in [
        "cat <\\\n<'DOC' >/dev/null\nignored\nDOC\n",
        "cat <\\\r\n<'DOC' >/dev/null\r\nignored\r\nDOC\r\n",
        "cat <\\\n\\\n<'DOC' >/dev/null\nignored\nDOC\n",
        "cat <\\\n<-DOC >/dev/null\nignored\nDOC\n",
    ] {
        assert!(has_opaque_heredoc_delimiter(source), "{source:?}");
    }
}

#[test]
fn inert_continuation_like_heredoc_text_remains_allowed() {
    for source in [
        "printf '%s' '<\\\n<' >/dev/null\n",
        "printf '%s' \"<\\\n<\" >/dev/null\n",
        "printf '%s' '<\\\\\n<' >/dev/null\n",
        "printf '%s' safe # <\\\n<'DOC'\n",
        "printf '%s' <\\\n <safe >/dev/null\n",
        "printf '%s' \\<\\\n<safe >/dev/null\n",
        "printf '%s' <\\\n\\<safe >/dev/null\n",
        "(( 1 <\\\n< 2 ))\n",
    ] {
        assert!(!has_opaque_heredoc_delimiter(source), "{source:?}");
    }
}

#[test]
fn comment_suffixes_do_not_delay_heredoc_bodies() {
    for suffix in ["&&", "||", "|", "|&", r"\"] {
        let source = format!("cat <<'DOC' # {suffix}\ncat <<$'E\\x4fF'\nDOC\nquality/run-lints\n");
        assert!(!has_opaque_heredoc_delimiter(&source), "{source}");
        let commands = without_noncommand_shell_data(&source);
        assert!(!commands.contains("E\\x4fF"), "{source}");
        assert!(commands.contains("quality/run-lints"), "{source}");
    }
}

#[test]
fn escaped_or_quoted_operator_suffixes_do_not_delay_heredoc_bodies() {
    for suffix in [r"\|", r"\&", "'|'", "\"||\""] {
        let source = format!("printf '%s' <<'DOC' {suffix}\ncat <<$'E\\x4fF'\nDOC\nquality/run-lints\n");
        assert!(!has_opaque_heredoc_delimiter(&source), "{source}");
        let commands = without_noncommand_shell_data(&source);
        assert!(!commands.contains("E\\x4fF"), "{source}");
        assert!(commands.contains("quality/run-lints"), "{source}");
    }
}

#[test]
fn payload_syntax_cannot_poison_opaque_delimiter_discovery() {
    let source = "cat <<'DOC' >/dev/null\n\"\nDOC\ncat <<$'E\\x4fF' >/dev/null\nignored\nEOF\n";
    assert!(has_opaque_heredoc_delimiter(source));
}

#[test]
fn context_dependent_delimiter_text_in_payload_is_inert() {
    let source = "cat <<'DOC' >/dev/null\ncat <<$'E\\x4fF'\nDOC\nprintf '%s\\n' safe\n";
    assert!(!has_opaque_heredoc_delimiter(source));
}

#[test]
fn multiple_and_tab_stripped_payloads_are_not_rescanned() {
    let source = "cat <<'ONE' <<-'TWO' >/dev/null\ncat <<$'BAD'\nONE\n\t\"\n\tTWO\nprintf '%s\\n' safe\n";
    assert!(!has_opaque_heredoc_delimiter(source));
}

#[test]
fn unterminated_heredoc_keeps_later_fake_openers_inert() {
    let source = "cat <<'DOC' >/dev/null\ncat <<$'E\\x4fF'\n\"\n";
    assert!(!has_opaque_heredoc_delimiter(source));
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
