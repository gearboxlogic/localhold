use super::super::assigned_variables;

#[test]
fn duplicate_function_definitions_fail_closed() {
    let source = "clearer() { declare -g +i value; }\nclearer() { :; }\ndeclare -i value=0\nclearer\nvalue=payload\n";
    assert!(assigned_variables(source).1);

    let distinct = "first() { :; }\nsecond() { :; }\nfirst\nsecond\n";
    assert!(!assigned_variables(distinct).1);
}

#[test]
fn inert_function_like_data_does_not_make_analysis_opaque() {
    for source in [
        "printf '%s\\n' 'fake() { :; }\nfake() { :; }'\n",
        "cat <<'DOC'\nfake() { :; }\nfake() { :; }\nDOC\nprintf '%s\\n' safe\n",
    ] {
        assert!(!assigned_variables(source).1, "{source}");
    }
}

#[test]
fn hyphenated_functions_and_unsupported_declarations_fail_closed() {
    let hyphenated = "pkg-config() { value=payload; }\ncaller() { local -i value=0; pkg-config; }\ncaller\n";
    assert!(assigned_variables(hyphenated).1);

    for source in [
        "if true; then callee() { value=payload; }; fi\ncaller() { local -i value=0; callee; }\ncaller\n",
        "return() { :; }\nsetter() { declare -g +i value; return; declare -gi value=0; }\ncallee() { value=payload; }\ndeclare +i value\nsetter\ncallee\n",
    ] {
        assert!(assigned_variables(source).1, "{source}");
    }
}

#[test]
fn recursive_effect_summaries_terminate_opaque() {
    let source = "left() { declare -gi value=0; right; }\nright() { declare -g +i value; left; }\nleft\n";
    assert!(assigned_variables(source).1);
}

#[test]
fn early_return_preserves_the_effect_at_the_exit_path() {
    let source = "callee() { value=payload; }\nsetter() { declare -gi value=0; return; declare -g +i value; }\ndeclare +i value\nsetter\ncallee\n";
    assert!(assigned_variables(source).1);

    let conditional = "callee() { value=payload; }\nsetter() { declare -gi value=0; false && return; declare -g +i value; }\ndeclare +i value\nsetter\ncallee\n";
    assert!(assigned_variables(conditional).1);

    let safe = "callee() { value=payload; }\nclearer() { declare -g +i value; return; declare -gi value=0; }\ndeclare -i value=0\nclearer\ncallee\n";
    assert!(!assigned_variables(safe).1);
}

#[test]
fn isolated_compound_effects_fail_closed_without_leaking() {
    for body in ["( setter; callee )", "{ setter; callee; } & wait", "( declare -gi value=0; callee )"] {
        let source = format!("callee() {{ value=payload; }}\nsetter() {{ declare -gi value=0; }}\nwrapper() {{ {body}; }}\ndeclare +i value\nwrapper\n");
        assert!(assigned_variables(&source).1, "{source}");
    }

    let safe = "callee() { value=payload; }\nnoop() { :; }\nwrapper() { ( noop ); }\ndeclare +i value\nwrapper\ncallee\n";
    assert!(!assigned_variables(safe).1);
}

#[test]
fn nested_substitutions_inherit_prior_global_effects() {
    for invocation in [
        "echo \"$(callee)\"",
        "echo \"$(printf '%s' \"$(callee)\")\"",
        "echo `callee`",
        "cat <(callee)",
        "cat >(callee)",
    ] {
        let source = format!("callee() {{ value=payload; }}\nwrapper() {{ declare -gi value=0; {invocation}; }}\ndeclare +i value\nwrapper\n");
        assert!(assigned_variables(&source).1, "{source}");
    }

    let safe = "callee() { value=payload; }\nwrapper() { declare -g +i value; echo \"$(callee)\"; }\ndeclare -i value=0\nwrapper\n";
    assert!(!assigned_variables(safe).1);
}

#[test]
fn dynamic_integer_scope_reaches_called_functions() {
    for source in [
        "callee() {\n  value=payload\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  local -I value\n  value=payload\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  declare -I value+=payload\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  typeset -I value=payload\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  value=payload\n}\ndeclare -i value=0\ncallee\n",
        "leaf() {\n  value=payload\n}\nmiddle() {\n  leaf\n}\ncaller() {\n  local -i value=0\n  middle\n}\ncaller\n",
        "callee() {\n  unset --unknown value 2>/dev/null || :\n  value=payload\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
    ] {
        let (_, opaque) = assigned_variables(source);
        assert!(opaque, "{source}");
    }
}

#[test]
fn assignment_builtins_follow_dynamic_integer_scope() {
    for assignment in ["printf -v value %s payload", "read -r value", "mapfile -t value", "readarray value", "getopts ab value"] {
        let source = format!("callee() {{\n  {assignment}\n}}\ncaller() {{\n  local -i value=0\n  callee\n}}\ncaller\n");
        let (_, opaque) = assigned_variables(&source);
        assert!(opaque, "{source}");
    }
}

#[test]
fn declaration_assignments_follow_dynamic_and_global_integer_scope() {
    for assignment in ["export value=payload", "readonly value=payload"] {
        let source = format!("callee() {{\n  {assignment}\n}}\ncaller() {{\n  local -i value=0\n  callee\n}}\ncaller\n");
        let (_, opaque) = assigned_variables(&source);
        assert!(opaque, "{source}");
    }

    for source in [
        "callee() {\n  declare -g value=payload\n}\ndeclare -i value=0\ncallee\n",
        "leaf() {\n  declare -g value=payload\n}\ncallee() {\n  leaf\n}\ndeclare -i value=0\ncallee\n",
        "declare -i value=0\nclear() {\n  declare -g +i value\n}\ncallee() {\n  declare -g value=payload\n}\ncallee\n",
        "set_integer() {\n  declare -gi value=0\n}\ncallee() {\n  declare -g value=payload\n}\nset_integer\ncallee\n",
    ] {
        let (_, opaque) = assigned_variables(source);
        assert!(opaque, "{source}");
    }
}

#[test]
fn force_global_attribute_effects_apply_when_functions_are_called() {
    for source in [
        "callee() { value=payload; }\nset_integer() { declare -gi value=0; }\ndeclare +i value\nset_integer\ncallee\n",
        "callee() { value=payload; }\nset_integer() { declare -gi value=0; }\nwrapper() { set_integer; }\ndeclare +i value\nwrapper\ncallee\n",
        "callee() { value=payload; }\nset_integer() { if true; then declare -gi value=0; fi; }\ndeclare +i value\nset_integer\ncallee\n",
        "callee() { value=payload; }\nset_integer() { declare -gi value=0; }\nwrapper() { set_integer; callee; }\ndeclare +i value\nwrapper\n",
        "set_and_assign() { declare -gi value=0; value=payload; }\ndeclare +i value\nset_and_assign\n",
    ] {
        let (_, opaque) = assigned_variables(source);
        assert!(opaque, "{source}");
    }
}

#[test]
fn force_global_plain_effects_clear_integer_state_when_called() {
    for source in [
        "callee() { value=payload; }\nclear_integer() { declare -g +i value; }\ndeclare -i value=0\nclear_integer\ncallee\n",
        "callee() { value=payload; }\nclear_integer() { declare -g +i value; }\nwrapper() { clear_integer; }\ndeclare -i value=0\nwrapper\ncallee\n",
        "callee() { value=payload; }\nset_integer() { declare -gi value=0; }\ndeclare +i value\necho \"$(set_integer)\"\ncallee\n",
        "callee() { value=payload; }\nset_integer() { declare -gi value=0; }\nwrapper() { callee; set_integer; }\ndeclare +i value\nwrapper\n",
        "callee() { value=payload; }\nset_integer() { declare -gi value=0; }\ndeclare +i value\necho \"$(callee; set_integer)\"\n",
        "assign_then_set() { value=payload; declare -gi value=0; }\ndeclare +i value\nassign_then_set\n",
        "callee() { value=payload; }\nwrapper() { declare -g +i value; callee; }\ndeclare -i value=0\nwrapper\n",
        "callee() { declare -g value=payload; }\nwrapper() { declare -g +i value; callee; }\ndeclare -i value=0\nwrapper\n",
    ] {
        let (_, opaque) = assigned_variables(source);
        assert!(!opaque, "{source}");
    }

    let conditional_clear = "callee() { value=payload; }\nclear_integer() { false && declare -g +i value; }\ndeclare -i value=0\nclear_integer\ncallee\n";
    assert!(assigned_variables(conditional_clear).1);

    let conditional_wrapper = "callee() { value=payload; }\nwrapper() { false && declare -g +i value; callee; }\ndeclare -i value=0\nwrapper\n";
    assert!(assigned_variables(conditional_wrapper).1);
}

#[test]
fn function_calls_inside_substitutions_inherit_dynamic_integer_scope() {
    for invocation in [
        "result=$(callee)",
        "result=\"$(printf '%s' \"$(callee)\")\"",
        "result=`callee`",
        "cat <(callee)",
        "cat >(callee)",
    ] {
        let source = format!("callee() {{ value=payload; }}\ncaller() {{ local -i value=0; {invocation}; }}\ncaller\n");
        let (_, opaque) = assigned_variables(&source);
        assert!(opaque, "{source}");
    }

    let ordered = "callee() { value=payload; }\nset_integer() { declare -gi value=0; }\ndeclare +i value\necho \"$(set_integer; callee)\"\n";
    assert!(assigned_variables(ordered).1);
}

#[test]
fn substitution_calls_keep_plain_and_numeric_controls_safe() {
    for source in [
        "callee() { value=payload; }\ndeclare value=plain\necho \"$(callee)\"\n",
        "callee() { value=-12; }\ndeclare -i value=0\necho \"$(callee)\"\n",
        "callee() { value=payload; }\ndeclare -i value=0\nprintf '%s' '$(callee)'\n",
    ] {
        let (_, opaque) = assigned_variables(source);
        assert!(!opaque, "{source}");
    }
}

#[test]
fn declaration_assignment_numeric_and_plain_controls_remain_safe() {
    for source in [
        "callee() {\n  export value=-12\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  readonly value=12\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  local value\n  export value=payload\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  declare -g +i value=payload\n}\ndeclare -i value=0\ncallee\n",
        "callee() {\n  declare -g value=-12\n}\ndeclare -i value=0\ncallee\n",
    ] {
        let (_, opaque) = assigned_variables(source);
        assert!(!opaque, "{source}");
    }
}

#[test]
fn explicit_plain_locals_and_earlier_calls_remain_safe() {
    for source in [
        "callee() {\n  local value\n  value=payload\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  local +i value\n  value=payload\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  unset -v value\n  value=payload\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  unset -- value\n  value=payload\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  unset value\n  value=payload\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  local -I value\n  value=payload\n}\ncaller() {\n  local value=plain\n  callee\n}\ncaller\n",
        "callee() {\n  local -I value=-12\n}\ncaller() {\n  local -i value=0\n  callee\n}\ncaller\n",
        "callee() {\n  value=payload\n}\ncallee\ndeclare -i value=0\n",
        "callee() {\n  value=payload\n}\ncommand callee 2>/dev/null || :\ndeclare -i value=0\n",
        "leaf() {\n  value=payload\n}\nmiddle() {\n  local value=plain\n  leaf\n}\ncaller() {\n  local -i value=0\n  middle\n}\ncaller\n",
    ] {
        let (_, opaque) = assigned_variables(source);
        assert!(!opaque, "{source}");
    }
}

#[test]
fn bindings_that_might_not_execute_do_not_mask_dynamic_integers() {
    for body in [
        "if false; then local value; fi\n  value=payload",
        "if false; then unset value; fi\n  value=payload",
        "while false; do local value; done\n  value=payload",
        "false && local value\n  value=payload",
        "local value | cat\n  value=payload",
        "local value |& cat\n  value=payload",
        "( local value )\n  value=payload",
    ] {
        let source = format!("callee() {{\n  {body}\n}}\ncaller() {{\n  local -i value=0\n  callee\n}}\ncaller\n");
        let (_, opaque) = assigned_variables(&source);
        assert!(opaque, "{source}");
    }
}

#[test]
fn continued_and_background_bindings_do_not_mask_dynamic_integers() {
    for body in [
        "false &&\n    local value\n  value=payload",
        "false && # still conditional\n    local value\n  value=payload",
        "false && local value &\n  wait\n  value=payload",
        "{ local value; :; } &\n  wait\n  value=payload",
    ] {
        let source = format!("callee() {{\n  {body}\n}}\ncaller() {{\n  local -i value=0\n  callee\n}}\ncaller\n");
        let (_, opaque) = assigned_variables(&source);
        assert!(opaque, "{source}");
    }
}

#[test]
fn local_integer_state_survives_uncertain_plain_transitions() {
    for transition in [
        "false && local +i value",
        "false && unset value",
        "local +i value | cat",
        "if false; then local +i value; fi",
    ] {
        let source = format!("check() {{\n  local -i value=0\n  {transition}\n  value=payload\n}}\ncheck\n");
        let (_, opaque) = assigned_variables(&source);
        assert!(opaque, "{source}");
    }

    for transition in ["local +i value", "unset value"] {
        let source = format!("check() {{\n  local -i value=0\n  {transition}\n  value=payload\n}}\ncheck\n");
        let (_, opaque) = assigned_variables(&source);
        assert!(!opaque, "{source}");
    }
}

#[test]
fn top_level_integer_state_survives_uncertain_plain_transitions() {
    for transition in ["false && declare +i value", "false && unset value", "declare +i value | cat"] {
        let source = format!("declare -i value=0\n{transition}\nvalue=payload\n");
        let (_, opaque) = assigned_variables(&source);
        assert!(opaque, "{source}");
    }

    let (_, opaque) = assigned_variables("declare -i value=0\ndeclare +i value\nvalue=payload\n");
    assert!(!opaque);
}

#[test]
fn conditional_bindings_propagate_through_transitive_calls() {
    for binding in ["if false; then local value; fi", "if false; then unset value; fi"] {
        let source = format!("leaf() {{\n  value=payload\n}}\ncallee() {{\n  {binding}\n  leaf\n}}\ncaller() {{\n  local -i value=0\n  callee\n}}\ncaller\n");
        let (_, opaque) = assigned_variables(&source);
        assert!(opaque, "{source}");
    }
}

#[test]
fn definitely_executed_and_isolated_plain_bindings_remain_safe() {
    for body in [
        "if false; then :; fi\n  local value\n  value=payload",
        "while false; do :; done\n  unset value\n  value=payload",
        "local value && :\n  value=payload",
        "( local value; value=payload )",
        "printf '%s\\n' safe | cat >/dev/null\n  local value\n  value=payload",
        "local value\n  sleep 0 &\n  wait\n  value=payload",
    ] {
        let source = format!("callee() {{\n  {body}\n}}\ncaller() {{\n  local -i value=0\n  callee\n}}\ncaller\n");
        let (_, opaque) = assigned_variables(&source);
        assert!(!opaque, "{source}");
    }
}

#[test]
fn heredoc_payload_quotes_cannot_hide_dynamic_integer_evaluation() {
    let source = "check() {\n  local -i value=0\n  cat <<'DOC' >/dev/null\n\"\nDOC\n  cat <<$'E\\x4fF' >/dev/null\n}\nEOF\n  value=payload\n}\n";
    let (_, opaque) = assigned_variables(source);
    assert!(opaque, "{source}");

    let inert = "cat <<'DOC' >/dev/null\ncat <<$'E\\x4fF'\nDOC\nprintf '%s\\n' safe\n";
    let (_, opaque) = assigned_variables(inert);
    assert!(!opaque, "{inert}");
}
