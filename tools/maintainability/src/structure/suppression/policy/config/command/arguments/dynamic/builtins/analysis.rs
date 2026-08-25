use std::collections::BTreeSet;

use super::{attributes, flow, function_scope, tokens};

pub(super) fn assigned_variables(source: &str) -> (BTreeSet<String>, bool) {
    let functions = tokens::declared_shell_functions(source);
    let commands = tokens::structured_source_commands(source);
    let dependencies = function_scope::Dependencies::analyze(&commands, &functions);
    let mut analysis = Analysis {
        names: BTreeSet::new(),
        opaque: tokens::has_opaque_heredoc_delimiter(source) || tokens::has_duplicate_shell_functions(source) || tokens::has_unsupported_shell_function(source),
        functions: &functions,
        dependencies: &dependencies,
    };
    analysis.commands(commands, &mut attributes::IntegerAttributes::default(), &mut flow::ExecutionFlow::default());
    (analysis.names, analysis.opaque)
}

struct Analysis<'a> {
    names: BTreeSet<String>,
    opaque: bool,
    functions: &'a BTreeSet<String>,
    dependencies: &'a function_scope::Dependencies,
}

impl Analysis<'_> {
    fn commands(&mut self, commands: Vec<tokens::StructuredCommand>, attributes: &mut attributes::IntegerAttributes, flow: &mut flow::ExecutionFlow) {
        for command in commands {
            attributes.enter_scope(&command, self.functions);
            let execution = flow.begin(&command);
            let uncertain = execution.uncertain || !execution.persistent;
            self.opaque |= attributes.ordinary_assignment_is_opaque(&command.words);
            if attributes.current_function().is_none() {
                self.nested_commands(&command.source, attributes);
            }
            self.opaque |= self.dependencies.call_is_opaque(&command, attributes);
            if let Some(index) = super::command_word_index(&command.words) {
                self.builtin(&command.words, index, attributes, uncertain);
            }
            self.dependencies.apply_call_effects(&command, attributes, execution.uncertain, execution.persistent);
            attributes.leave_scope(&command);
            flow.finish(&command);
        }
    }

    fn builtin(&mut self, words: &[String], index: usize, attributes: &mut attributes::IntegerAttributes, uncertain: bool) {
        let arguments = &words[index.saturating_add(1)..];
        let command = words[index].trim_matches(['(', ')', '{', '}']).to_ascii_lowercase();
        match command.as_str() {
            "declare" | "local" | "typeset" => {
                self.opaque |= super::declaration_has_opaque_target(arguments, true, true);
                self.opaque |= attributes.declaration_initializer_is_opaque(&command, arguments);
                attributes.update_declaration(&command, arguments, uncertain);
            }
            "readonly" | "export" => {
                self.opaque |= super::declaration_has_opaque_target(arguments, false, false);
                self.opaque |= attributes.preserving_initializer_is_opaque(&command, arguments);
            }
            "unset" => attributes.update_unset(arguments, uncertain),
            "printf" | "read" | "readarray" | "mapfile" | "getopts" | "wait" => {
                self.assignment_builtin(&command, arguments, attributes);
            }
            _ => {}
        }
    }

    fn assignment_builtin(&mut self, command: &str, arguments: &[String], attributes: &attributes::IntegerAttributes) {
        let mut assigned = BTreeSet::new();
        match command {
            "printf" => super::collect_printf_target(arguments, &mut assigned, &mut self.opaque),
            "read" | "readarray" | "mapfile" => super::collect_read_targets(command, arguments, &mut assigned, &mut self.opaque),
            "getopts" => super::collect_getopts_target(arguments, &mut assigned, &mut self.opaque),
            "wait" => super::collect_wait_target(arguments, &mut assigned, &mut self.opaque),
            _ => unreachable!("matched assignment builtin"),
        }
        if command != "wait" {
            self.opaque |= assigned.iter().any(|name| attributes.is_integer(name));
        }
        self.names.extend(assigned);
    }

    fn nested_commands(&mut self, source: &str, attributes: &attributes::IntegerAttributes) {
        let (mut nested, _) = tokens::command_substitution_commands(source, true);
        nested.extend(tokens::process_substitution_commands(source).0);
        for source in nested {
            self.commands(tokens::structured_source_commands(&source), &mut attributes.clone(), &mut flow::ExecutionFlow::default());
        }
    }
}
