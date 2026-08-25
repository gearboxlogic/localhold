mod expansion;
mod forbidden;
mod syntax;

pub(super) struct Inventory {
    pub(super) definitions: Vec<super::ShellFunctionDefinition>,
    pub(super) unsupported: bool,
}

pub(super) fn inventory(source: &str) -> Inventory {
    let expansion_source = super::shell_expansion_source(source);
    let unsupported_expansion = expansion::has_current_shell_substitution(&expansion_source);
    let command_source = super::heredoc::command_source(source);
    let unsupported_raw_process =
        super::substitution::without_active_continuations(&command_source).map_or(true, |command_source| unsupported_process_substitution(&command_source));
    let active = super::without_noncommand_shell_data(source);
    let mut inventory = inventory_from_active_source(&active);
    inventory.unsupported |= unsupported_expansion || unsupported_raw_process;
    inventory
}

fn unsupported_process_substitution(source: &str) -> bool {
    let (processes, malformed) = super::process_substitution_commands(source);
    malformed || processes.into_iter().any(|command| nested_source_is_unsupported(&command))
}

fn nested_source_is_unsupported(source: &str) -> bool {
    let nested = Scanner::new(source).scan();
    nested.unsupported || expansion::has_current_shell_substitution(source) || !nested.definitions.is_empty()
}

pub(super) fn inventory_from_active_source(source: &str) -> Inventory {
    let mut inventory = Scanner::new(source).scan();
    inventory.unsupported |= expansion::has_current_shell_substitution(source);
    let (mut nested, malformed_commands) = super::command_substitution_commands(source, true);
    let (processes, malformed_processes) = super::process_substitution_commands(source);
    nested.extend(processes);
    inventory.unsupported |= malformed_commands || malformed_processes;
    inventory.unsupported |= nested.into_iter().any(|command| nested_source_is_unsupported(&command));
    inventory
}

struct Scanner<'a> {
    source: &'a str,
    index: usize,
    command_start: usize,
    quote: Option<char>,
    escaped: bool,
    comment: bool,
    ansi_c_quote: Option<()>,
    parenthesis_depth: usize,
    control_depth: usize,
    brace_ends: Vec<usize>,
    pending_header: bool,
    inventory: Inventory,
}

impl<'a> Scanner<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            source,
            index: 0,
            command_start: 0,
            quote: None,
            escaped: false,
            comment: false,
            ansi_c_quote: None,
            parenthesis_depth: 0,
            control_depth: 0,
            brace_ends: Vec::new(),
            pending_header: false,
            inventory: Inventory {
                definitions: Vec::new(),
                unsupported: false,
            },
        }
    }

    fn scan(mut self) -> Inventory {
        while let Some(character) = self.current() {
            if self.brace_ends.last() == Some(&self.index) {
                self.brace_ends.pop();
            }
            if self.advance_over_substitution() {
                continue;
            }
            if self.advance_lexical_state(character) {
                continue;
            }
            if character == '{' {
                self.observe_brace_definition();
            } else if character == '(' {
                self.observe_non_brace_definition();
            }
            if character == '(' {
                self.parenthesis_depth += 1;
            } else if character == ')' {
                self.parenthesis_depth = self.parenthesis_depth.saturating_sub(1);
            }
            if matches!(character, '\n' | ';' | '&' | '|') {
                self.finish_command();
                self.command_start = self.index + character.len_utf8();
            }
            self.index += character.len_utf8();
        }
        self.inventory
    }

    fn current(&self) -> Option<char> {
        self.source[self.index..].chars().next()
    }

    fn advance_over_substitution(&mut self) -> bool {
        if !self.substitution_is_active() {
            return false;
        }
        let Some(span) = super::substitution::span_at(self.source, self.index) else {
            return false;
        };
        if let Ok(span) = span {
            self.index = span.end + 1;
        } else {
            self.inventory.unsupported = true;
            self.index = self.source.len();
        }
        true
    }

    fn substitution_is_active(&self) -> bool {
        !self.escaped && !self.comment && self.quote != Some('\'')
    }

    fn advance_lexical_state(&mut self, character: char) -> bool {
        if self.comment {
            self.comment = character != '\n';
            self.index += character.len_utf8();
            if !self.comment {
                self.finish_command();
                self.command_start = self.index;
            }
            return true;
        }
        if self.escaped {
            self.escaped = false;
            self.index += character.len_utf8();
            return true;
        }
        if character == '\\' && (self.quote != Some('\'') || self.ansi_c_quote.is_some()) {
            self.escaped = true;
            self.index += 1;
            return true;
        }
        if matches!(character, '\'' | '"' | '`') {
            if self.quote.is_none() {
                self.ansi_c_quote = (character == '\'' && syntax::ansi_c_quote_opener(self.source, self.index)).then_some(());
            } else if self.quote == Some(character) {
                self.ansi_c_quote = None;
            }
            self.quote = syntax::updated_quote(self.quote, character);
            self.index += character.len_utf8();
            return true;
        }
        if self.quote.is_none() && character == '#' && syntax::starts_comment(self.source, self.index) {
            if self.pending_header || syntax::parse_header(&self.source[self.command_start..self.index]).is_some() {
                self.inventory.unsupported = true;
                self.pending_header = false;
            }
            self.comment = true;
            self.index += 1;
            return true;
        }
        if self.quote.is_some() {
            self.index += character.len_utf8();
            return true;
        }
        false
    }

    fn observe_brace_definition(&mut self) {
        let Some(header) = self.declaration_header() else {
            let command = &self.source[self.command_start..self.index];
            let normalized = syntax::without_continuations(command);
            self.inventory.unsupported |= self.pending_header || syntax::looks_like_function_header(command) || syntax::parse_header(&normalized).is_some();
            self.pending_header = false;
            self.push_brace_end();
            return;
        };
        self.pending_header = false;
        self.inventory.unsupported |= header.prefixed
            || self.parenthesis_depth > 0
            || self.control_depth > 0
            || !self.brace_ends.is_empty()
            || syntax::preceded_by_nonsequential_operator(self.source, self.command_start)
            || forbidden::name(header.name);
        let Some(end) = syntax::matching_brace(self.source, self.index) else {
            self.inventory.definitions.push(super::ShellFunctionDefinition {
                name: header.name.to_owned(),
                body: String::new(),
            });
            return;
        };
        self.inventory.definitions.push(super::ShellFunctionDefinition {
            name: header.name.to_owned(),
            body: self.source[self.index + 1..end].to_owned(),
        });
        self.inventory.unsupported |= syntax::has_unsupported_body_suffix(self.source, end);
        self.brace_ends.push(end);
    }

    fn observe_non_brace_definition(&mut self) {
        let current = self.source[self.command_start..self.index].trim();
        let header = syntax::parse_header(current).or_else(|| {
            current
                .is_empty()
                .then(|| syntax::previous_command(self.source, self.command_start))
                .flatten()
                .and_then(syntax::parse_header)
        });
        let Some(header) = header else {
            return;
        };
        self.inventory.unsupported = true;
        self.inventory.definitions.push(super::ShellFunctionDefinition {
            name: header.name.to_owned(),
            body: String::new(),
        });
    }

    fn declaration_header(&self) -> Option<syntax::Header<'a>> {
        let current = self.source[self.command_start..self.index].trim();
        syntax::parse_header(current).or_else(|| {
            current
                .is_empty()
                .then(|| syntax::previous_command(self.source, self.command_start))
                .flatten()
                .and_then(syntax::parse_header)
        })
    }

    fn push_brace_end(&mut self) {
        if structural_brace(self.source, self.index)
            && let Some(end) = syntax::matching_brace(self.source, self.index)
        {
            self.brace_ends.push(end);
        }
    }

    fn finish_command(&mut self) {
        let command = self.source[self.command_start..self.index].trim();
        if self.pending_header {
            self.inventory.unsupported = true;
            self.pending_header = false;
        }
        self.pending_header |= syntax::parse_header(command).is_some();
        if self.parenthesis_depth == 0 && self.brace_ends.is_empty() {
            update_control_depth(command, &mut self.control_depth);
        }
    }
}

fn structural_brace(source: &str, index: usize) -> bool {
    let previous = source[..index].chars().next_back();
    let next = source[index + 1..].chars().next();
    previous != Some('$')
        && previous.is_none_or(|character| character.is_whitespace() || matches!(character, ';' | '&' | '|' | '(' | ')'))
        && next.is_none_or(|character| character.is_whitespace() || matches!(character, ';' | '&' | '|' | '}'))
}

fn update_control_depth(command: &str, depth: &mut usize) {
    let word = command
        .split_whitespace()
        .map(|word| word.trim_matches([';', '{', '}']))
        .find(|word| !matches!(*word, "" | "!"));
    if matches!(word, Some("fi" | "done" | "esac")) {
        *depth = depth.saturating_sub(1);
    } else if matches!(word, Some("if" | "while" | "until" | "for" | "select" | "case")) {
        *depth += 1;
    }
}
