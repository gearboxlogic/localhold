use super::tokens;

#[derive(Default)]
pub(super) struct ExecutionFlow {
    control_depth: usize,
    subshell_depth: usize,
}

impl ExecutionFlow {
    pub(super) fn begin(&mut self, command: &tokens::StructuredCommand) -> ExecutionState {
        if control_words(command).any(|word| matches!(word, "fi" | "done" | "esac")) {
            self.control_depth = self.control_depth.saturating_sub(1);
        }
        self.subshell_depth += command.open_subshells;
        ExecutionState {
            uncertain: self.control_depth > 0 || command.conditionally_executed,
            persistent: self.subshell_depth == 0 && !command.isolated,
        }
    }

    pub(super) fn finish(&mut self, command: &tokens::StructuredCommand) {
        self.subshell_depth = self.subshell_depth.saturating_sub(command.close_subshells);
        if control_words(command).any(|word| matches!(word, "if" | "while" | "until" | "for" | "select" | "case")) {
            self.control_depth += 1;
        }
    }
}

pub(super) struct ExecutionState {
    pub(super) uncertain: bool,
    pub(super) persistent: bool,
}

fn control_words(command: &tokens::StructuredCommand) -> impl Iterator<Item = &str> {
    let end = super::command_word_index(&command.words).map_or(command.words.len(), |index| index + 1);
    command.words[..end].iter().map(|word| word.trim_matches(['(', ')', '{', '}', ';']))
}
