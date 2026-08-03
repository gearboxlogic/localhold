use std::ops::Range;

pub(super) struct Call {
    pub(super) name: String,
    pub(super) opening_parenthesis: usize,
}

pub(super) struct CallScanner {
    characters: Vec<char>,
    index: usize,
    opaque_formatted_process_expression: bool,
}

impl CallScanner {
    pub(super) fn new(source: &str) -> Self {
        Self {
            characters: source.chars().collect(),
            index: 0,
            opaque_formatted_process_expression: false,
        }
    }

    pub(super) fn next(&mut self) -> Option<Call> {
        while self.index < self.characters.len() {
            if self.characters[self.index] == '#' {
                self.index = self.comment_end(self.index);
                continue;
            }
            if let Some(literal) = self.string_literal(self.index) {
                self.record_formatted_process_expression(&literal);
                self.index = literal.end;
                continue;
            }
            if !identifier_start(self.characters[self.index]) {
                self.index += 1;
                continue;
            }
            let start = self.index;
            let (name, end) = self.qualified_name(start);
            self.index = end;
            let opening_parenthesis = self.skip_whitespace(self.index);
            if self.characters.get(opening_parenthesis) == Some(&'(') {
                self.index = opening_parenthesis + 1;
                return Some(Call { name, opening_parenthesis });
            }
        }
        None
    }

    pub(super) const fn has_opaque_formatted_process_expression(&self) -> bool {
        self.opaque_formatted_process_expression
    }

    pub(super) fn arguments(&self, opening: usize) -> Option<Vec<Range<usize>>> {
        self.split(opening + 1, ')')
    }

    pub(super) fn sequence(&self, expression: Range<usize>) -> Option<Vec<Range<usize>>> {
        let expression = self.trim(expression);
        let opening = *self.characters.get(expression.start)?;
        let closing = match opening {
            '[' => ']',
            '(' => ')',
            _ => return None,
        };
        (self.characters.get(expression.end.checked_sub(1)?) == Some(&closing)).then(|| self.split(expression.start + 1, closing))?
    }

    pub(super) fn compact(&self, expression: Range<usize>) -> String {
        self.characters[expression].iter().filter(|character| !character.is_whitespace()).collect()
    }

    pub(super) fn literal(&self, expression: Range<usize>) -> Option<String> {
        let expression = self.trim(expression);
        let end = self.string_end(expression.start)?;
        if end != expression.end {
            return None;
        }
        let mut quote = expression.start;
        while quote < expression.end && quote - expression.start < 2 && matches!(self.characters[quote].to_ascii_lowercase(), 'r' | 'u') {
            quote += 1;
        }
        let delimiter = *self.characters.get(quote).filter(|character| matches!(character, '\'' | '"'))?;
        let width = if self.characters.get(quote + 1) == Some(&delimiter) && self.characters.get(quote + 2) == Some(&delimiter) {
            3
        } else {
            1
        };
        let end = expression.end.checked_sub(width)?;
        let value = self.characters[quote + width..end].iter().collect::<String>();
        (!value.contains('\\')).then_some(value)
    }

    fn split(&self, start: usize, closing: char) -> Option<Vec<Range<usize>>> {
        let mut ranges = Vec::new();
        let mut index = start;
        let mut item_start = start;
        let mut depth = 0_u32;
        while index < self.characters.len() {
            if let Some(end) = self.string_end(index) {
                index = end;
                continue;
            }
            match self.characters[index] {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth > 0 => depth -= 1,
                character if character == closing && depth == 0 => {
                    self.push_nonempty(&mut ranges, item_start..index);
                    return Some(ranges);
                }
                ',' if depth == 0 => {
                    self.push_nonempty(&mut ranges, item_start..index);
                    item_start = index + 1;
                }
                _ => {}
            }
            index += 1;
        }
        None
    }

    fn string_end(&self, start: usize) -> Option<usize> {
        self.string_literal(start).map(|literal| literal.end)
    }

    fn string_literal(&self, start: usize) -> Option<StringLiteral> {
        if start > 0 && identifier_character(self.characters[start - 1]) {
            return None;
        }
        let mut quote = start;
        let mut formatted = false;
        while quote < self.characters.len() && quote - start < 3 && matches!(self.characters[quote].to_ascii_lowercase(), 'b' | 'f' | 'r' | 'u') {
            formatted |= self.characters[quote].eq_ignore_ascii_case(&'f');
            quote += 1;
        }
        let delimiter = *self.characters.get(quote).filter(|character| matches!(character, '\'' | '"'))?;
        let triple = self.characters.get(quote + 1) == Some(&delimiter) && self.characters.get(quote + 2) == Some(&delimiter);
        let width = if triple { 3 } else { 1 };
        let mut index = quote + width;
        while index < self.characters.len() {
            if self.characters[index] == '\\' {
                index = index.saturating_add(2);
            } else if formatted && self.characters[index] == '{' {
                index = self.formatted_expression_end(index)?;
            } else if self.characters[index] == delimiter && (!triple || self.characters.get(index + 1) == Some(&delimiter) && self.characters.get(index + 2) == Some(&delimiter)) {
                return Some(StringLiteral {
                    end: index + width,
                    content: quote + width..index,
                    formatted,
                });
            } else {
                index += 1;
            }
        }
        None
    }

    fn record_formatted_process_expression(&mut self, literal: &StringLiteral) {
        self.opaque_formatted_process_expression |= literal.formatted && self.formatted_string_has_process_expression(literal);
    }

    fn formatted_expression_end(&self, opening: usize) -> Option<usize> {
        if self.characters.get(opening + 1) == Some(&'{') {
            return Some(opening + 2);
        }
        self.f_expression_end(opening + 1, self.characters.len()).map(|closing| closing + 1)
    }

    fn formatted_string_has_process_expression(&self, literal: &StringLiteral) -> bool {
        let mut index = literal.content.start;
        while let Some(expression_start) = self.next_f_expression(index, literal.content.end) {
            let Some(expression_end) = self.f_expression_end(expression_start, literal.content.end) else {
                return self.expression_has_process_call(expression_start..literal.content.end);
            };
            if self.expression_has_process_call(expression_start..expression_end) {
                return true;
            }
            index = expression_end + 1;
        }
        false
    }

    fn next_f_expression(&self, mut index: usize, end: usize) -> Option<usize> {
        while index < end {
            match (self.characters[index], self.characters.get(index + 1)) {
                ('{', Some('{')) => index += 2,
                ('{', _) => return Some(index + 1),
                _ => index += 1,
            }
        }
        None
    }

    fn f_expression_end(&self, start: usize, end: usize) -> Option<usize> {
        let mut depth = 1_u32;
        let mut index = start;
        while index < end {
            if self.characters[index] == '#' {
                index = self.comment_end(index);
                continue;
            }
            if let Some(literal) = self.string_literal(index) {
                index = literal.end;
                continue;
            }
            match self.characters[index] {
                '{' => depth += 1,
                '}' if depth == 1 => return Some(index),
                '}' => depth -= 1,
                _ => {}
            }
            index += 1;
        }
        None
    }

    fn expression_has_process_call(&self, expression: Range<usize>) -> bool {
        let source = self.characters[expression].iter().collect::<String>();
        if super::super::has_opaque_process_bindings(&source) || super::super::has_direct_dynamic_process_resolution(&source) {
            return true;
        }
        let mut scanner = Self::new(&source);
        while let Some(call) = scanner.next() {
            if process_call(&call.name) {
                return true;
            }
        }
        scanner.has_opaque_formatted_process_expression()
    }

    fn comment_end(&self, start: usize) -> usize {
        self.characters[start..]
            .iter()
            .position(|character| *character == '\n')
            .map_or(self.characters.len(), |offset| start + offset + 1)
    }

    fn trim(&self, mut range: Range<usize>) -> Range<usize> {
        while self.characters.get(range.start).is_some_and(|character| character.is_whitespace()) {
            range.start += 1;
        }
        while range.end > range.start && self.characters.get(range.end - 1).is_some_and(|character| character.is_whitespace()) {
            range.end -= 1;
        }
        range
    }

    fn nonempty(&self, range: Range<usize>) -> Option<Range<usize>> {
        let range = self.trim(range);
        (!range.is_empty()).then_some(range)
    }

    fn push_nonempty(&self, ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
        ranges.extend(self.nonempty(range));
    }

    fn skip_whitespace(&self, mut index: usize) -> usize {
        while self.characters.get(index).is_some_and(|character| character.is_whitespace()) {
            index += 1;
        }
        index
    }

    fn qualified_name(&self, start: usize) -> (String, usize) {
        let mut end = self.identifier_end(start);
        let mut name = self.characters[start..end].iter().collect::<String>();
        while let Some((component, component_end)) = self.dotted_component(end) {
            name.push('.');
            name.extend(self.characters[component..component_end].iter());
            end = component_end;
        }
        (name, end)
    }

    fn dotted_component(&self, end: usize) -> Option<(usize, usize)> {
        let dot = self.skip_whitespace(end);
        (self.characters.get(dot) == Some(&'.')).then_some(())?;
        let component = self.skip_whitespace(dot + 1);
        self.characters.get(component).is_some_and(|character| identifier_start(*character)).then(|| {
            let component_end = self.identifier_end(component);
            (component, component_end)
        })
    }

    fn identifier_end(&self, start: usize) -> usize {
        let mut end = start + 1;
        while self.characters.get(end).is_some_and(|character| identifier_character(*character)) {
            end += 1;
        }
        end
    }
}

#[derive(Clone)]
struct StringLiteral {
    end: usize,
    content: Range<usize>,
    formatted: bool,
}

fn process_call(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    super::process_kind(&name).is_some()
        || matches!(
            name.as_str(),
            "os.chdir" | "os.fchdir" | "os.putenv" | "os.unsetenv" | "posix.chdir" | "posix.fchdir" | "posix.putenv" | "posix.unsetenv" | "contextlib.chdir"
        )
        || name.starts_with("os.environ.")
}

fn identifier_start(character: char) -> bool {
    character == '_' || character.is_alphabetic()
}

fn identifier_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::CallScanner;

    #[test]
    fn formatted_string_before_process_call_does_not_hide_it() {
        for source in [
            r#"
environment["PATH"] = f"{fake_bin}{os.pathsep}{environment['PATH']}"
subprocess.run(
    ["script/check-time-abstraction.sh"],
    check=True,
)
"#,
            r#"
environment["PATH"] = f"{environment["PATH"]}"
subprocess.run(["script/check-time-abstraction.sh"], check=True)
"#,
        ] {
            let mut scanner = CallScanner::new(source);
            let mut names = Vec::new();
            while let Some(call) = scanner.next() {
                names.push(call.name);
            }
            assert!(names.iter().any(|name| name == "subprocess.run"), "{names:?}");
            assert!(!scanner.has_opaque_formatted_process_expression());
        }
    }

    #[test]
    fn formatted_process_and_resolution_expressions_fail_closed() {
        for source in [
            r#"message = f"{posix.system('sh quality/hidden.txt')}""#,
            r#"message = f"{os.chdir('quality')}""#,
            r#"message = f"{os.environ.update({'PATH': 'quality'})}""#,
            r#"message = f"{(process := os)}""#,
            r#"message = f"{sys.modules['os'].system('sh quality/hidden.txt')}""#,
            r#"message = f"{globals()['os'].system('sh quality/hidden.txt')}""#,
            r#"message = f"{os.__getattribute__('system')('sh quality/hidden.txt')}""#,
        ] {
            let mut scanner = CallScanner::new(source);
            while scanner.next().is_some() {}
            assert!(scanner.has_opaque_formatted_process_expression(), "{source}");
        }
    }
}
