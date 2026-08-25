mod reviewed;

use std::ops::Range;

pub(super) fn has_dynamic_code(source: &str) -> bool {
    has_non_ascii_code(source) || Scanner::new(source).has_dynamic_code()
}

pub(super) fn has_non_ascii_code(source: &str) -> bool {
    Scanner::new(source).has_non_ascii_code()
}

pub(super) fn formatted_code_expressions(content: &str) -> Option<Vec<String>> {
    Scanner::new(content).f_string_code_expressions(0, content.chars().count())
}

pub(super) fn is_reviewed_dynamic_code_surface(path: &str, source: &str) -> bool {
    reviewed::matches(path, source)
}

struct Scanner {
    characters: Vec<char>,
    index: usize,
}

impl Scanner {
    fn new(source: &str) -> Self {
        Self {
            characters: source.chars().collect(),
            index: 0,
        }
    }

    fn has_dynamic_code(mut self) -> bool {
        while self.index < self.characters.len() {
            if self.advance() {
                return true;
            }
        }
        false
    }

    fn has_non_ascii_code(mut self) -> bool {
        while self.index < self.characters.len() {
            if self.advance_non_ascii_code() {
                return true;
            }
        }
        false
    }

    fn advance_non_ascii_code(&mut self) -> bool {
        if self.characters[self.index] == '#' {
            self.skip_comment();
            return false;
        }
        if let Some(literal) = self.string_literal(self.index) {
            self.index = literal.end;
            return literal.formatted && self.f_string_has_non_ascii_code(literal.content_start, literal.content_end);
        }
        let non_ascii = !self.characters[self.index].is_ascii();
        self.index += 1;
        non_ascii
    }

    fn advance(&mut self) -> bool {
        if self.characters[self.index] == '#' {
            self.skip_comment();
            return false;
        }
        if let Some(literal) = self.string_literal(self.index) {
            return self.advance_string(literal);
        }
        if is_identifier_start(self.characters[self.index]) {
            return self.advance_identifier();
        }
        self.index += 1;
        false
    }

    fn advance_string(&mut self, literal: StringLiteral) -> bool {
        self.index = literal.end;
        literal.formatted && self.f_string_has_dynamic_code(literal.content_start, literal.content_end)
    }

    fn advance_identifier(&mut self) -> bool {
        let start = self.index;
        let (path, end) = self.dotted_identifier(self.index);
        self.index = end;
        dynamic_path(&path) || path.first().is_some_and(|name| name == "lambda") && self.lambda_class_reference(start)
    }

    fn lambda_class_reference(&self, start: usize) -> bool {
        let Some(mut opening) = self.previous_non_whitespace(start).filter(|index| self.characters[*index] == '(') else {
            return false;
        };
        loop {
            let Some(closing) = self.matching_group_end(opening) else {
                return true;
            };
            let suffix = self.skip_whitespace(closing + 1);
            if self.attribute_at(suffix, "__class__") {
                return true;
            }
            let Some(outer) = self.previous_non_whitespace(opening).filter(|index| self.characters[*index] == '(') else {
                return false;
            };
            if self.characters.get(suffix) != Some(&')') || self.matching_group_end(outer) != Some(suffix) {
                return false;
            }
            opening = outer;
        }
    }

    fn matching_group_end(&self, opening: usize) -> Option<usize> {
        let mut expected = vec![')'];
        let mut index = opening + 1;
        while index < self.characters.len() {
            if self.characters[index] == '#' {
                index = self.comment_end(index);
                continue;
            }
            if let Some(literal) = self.string_literal(index) {
                index = literal.end;
                continue;
            }
            match self.characters[index] {
                '(' => expected.push(')'),
                '[' => expected.push(']'),
                '{' => expected.push('}'),
                ')' | ']' | '}' => match close_group(&mut expected, self.characters[index]) {
                    GroupClose::Mismatch => return None,
                    GroupClose::Complete => return Some(index),
                    GroupClose::Nested => {}
                },
                _ => {}
            }
            index += 1;
        }
        None
    }

    fn attribute_at(&self, start: usize, name: &str) -> bool {
        let Some(after_dot) = self.characters.get(start).filter(|character| **character == '.').map(|_| self.skip_whitespace(start + 1)) else {
            return false;
        };
        let end = after_dot.checked_add(name.chars().count());
        end.and_then(|end| self.characters.get(after_dot..end))
            .is_some_and(|characters| characters.iter().copied().eq(name.chars()))
            && !end.and_then(|end| self.characters.get(end)).is_some_and(|character| is_identifier_character(*character))
    }

    fn previous_non_whitespace(&self, start: usize) -> Option<usize> {
        self.characters[..start].iter().rposition(|character| !character.is_whitespace())
    }

    fn skip_comment(&mut self) {
        while self.characters.get(self.index).is_some_and(|character| *character != '\n') {
            self.index += 1;
        }
    }

    fn dotted_identifier(&self, start: usize) -> (Vec<String>, usize) {
        let mut path = Vec::new();
        let mut index = start;
        loop {
            let end = self.identifier_end(index);
            path.push(self.characters[index..end].iter().collect());
            index = self.skip_whitespace(end);
            if self.characters.get(index) != Some(&'.') {
                return (path, end);
            }
            let next = self.skip_whitespace(index + 1);
            if !self.characters.get(next).is_some_and(|character| is_identifier_start(*character)) {
                return (path, end);
            }
            index = next;
        }
    }

    fn identifier_end(&self, mut index: usize) -> usize {
        index += 1;
        while self.characters.get(index).is_some_and(|character| is_identifier_character(*character)) {
            index += 1;
        }
        index
    }

    fn string_literal(&self, start: usize) -> Option<StringLiteral> {
        if start > 0 && is_identifier_character(self.characters[start - 1]) {
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
                index = self.formatted_content_end(index);
            } else if self.characters[index] == delimiter && (!triple || self.characters.get(index + 1) == Some(&delimiter) && self.characters.get(index + 2) == Some(&delimiter)) {
                return Some(StringLiteral {
                    end: index + width,
                    content_start: quote + width,
                    content_end: index,
                    formatted,
                });
            } else {
                index += 1;
            }
        }
        Some(StringLiteral {
            end: self.characters.len(),
            content_start: quote + width,
            content_end: self.characters.len(),
            formatted,
        })
    }

    fn formatted_content_end(&self, opening: usize) -> usize {
        if self.characters.get(opening + 1) == Some(&'{') {
            return opening + 2;
        }
        self.f_expression_end(opening + 1, self.characters.len())
            .map_or(self.characters.len(), |closing| closing + 1)
    }

    fn f_string_has_dynamic_code(&self, start: usize, end: usize) -> bool {
        self.f_string_code_expressions(start, end)
            .is_none_or(|expressions| expressions.iter().any(|expression| Self::new(expression).has_dynamic_code()))
    }

    fn f_string_has_non_ascii_code(&self, start: usize, end: usize) -> bool {
        self.f_string_code_expressions(start, end)
            .is_none_or(|expressions| expressions.iter().any(|expression| Self::new(expression).has_non_ascii_code()))
    }

    fn f_string_code_expressions(&self, start: usize, end: usize) -> Option<Vec<String>> {
        let mut expressions = Vec::new();
        self.collect_f_string_code_expressions(start, end, true, &mut expressions)?;
        Some(expressions)
    }

    fn collect_f_string_code_expressions(&self, start: usize, end: usize, escaped_braces: bool, expressions: &mut Vec<String>) -> Option<()> {
        let mut index = start;
        while let Some(expression_start) = self.next_f_expression(index, end, escaped_braces) {
            let field_end = self.f_expression_end(expression_start, end)?;
            let field = self.f_field_parts(expression_start, field_end)?;
            let expression = self.characters[field.expression].iter().collect::<String>();
            expressions.push(super::lexical::normalize_continuations(&expression));
            if let Some(format_spec) = field.format_spec {
                self.collect_f_string_code_expressions(format_spec.start, format_spec.end, false, expressions)?;
            }
            index = field_end + 1;
        }
        Some(())
    }

    fn f_field_parts(&self, start: usize, end: usize) -> Option<FieldParts> {
        let mut index = start;
        let mut expression_end = None;
        let mut grouping = Grouping::default();
        while index < end {
            if self.characters[index] == '#' {
                index = self.comment_end(index);
                continue;
            }
            if let Some(literal) = self.string_literal(index) {
                index = literal.end;
                continue;
            }
            match self.field_marker(index, &grouping) {
                Some(FieldMarker::FormatSpec) => return self.field_parts_with_spec(start, expression_end.unwrap_or(index), index + 1..end),
                Some(FieldMarker::Conversion) => {
                    expression_end.get_or_insert(index);
                    index += 1;
                    continue;
                }
                None => {}
            }
            grouping.update(self.characters[index]);
            index += 1;
        }
        let expression = start..expression_end.unwrap_or(end);
        self.nonempty_expression(expression).map(|expression| FieldParts { expression, format_spec: None })
    }

    fn field_marker(&self, index: usize, grouping: &Grouping) -> Option<FieldMarker> {
        if !grouping.is_top_level() {
            return None;
        }
        match (self.characters[index], self.characters.get(index + 1)) {
            (':', _) => Some(FieldMarker::FormatSpec),
            ('!', next) if next != Some(&'=') => Some(FieldMarker::Conversion),
            _ => None,
        }
    }

    fn field_parts_with_spec(&self, start: usize, expression_end: usize, format_spec: Range<usize>) -> Option<FieldParts> {
        self.nonempty_expression(start..expression_end).map(|expression| FieldParts {
            expression,
            format_spec: Some(format_spec),
        })
    }

    fn nonempty_expression(&self, mut expression: Range<usize>) -> Option<Range<usize>> {
        while self.characters.get(expression.start).is_some_and(|character| character.is_whitespace()) {
            expression.start += 1;
        }
        while expression.end > expression.start && self.characters.get(expression.end - 1).is_some_and(|character| character.is_whitespace()) {
            expression.end -= 1;
        }
        (!expression.is_empty()).then_some(expression)
    }

    fn next_f_expression(&self, mut index: usize, end: usize, escaped_braces: bool) -> Option<usize> {
        while index < end {
            match (self.characters[index], self.characters.get(index + 1)) {
                ('{', Some('{')) if escaped_braces => index += 2,
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
            let token = self.expression_token(index);
            if token.brace_delta < 0 && depth == 1 {
                return Some(index);
            }
            depth = depth.saturating_add_signed(i32::from(token.brace_delta));
            index = token.next;
        }
        None
    }

    fn expression_token(&self, index: usize) -> ExpressionToken {
        if self.characters[index] == '#' {
            return ExpressionToken {
                next: self.comment_end(index),
                brace_delta: 0,
            };
        }
        if let Some(literal) = self.string_literal(index) {
            return ExpressionToken {
                next: literal.end,
                brace_delta: 0,
            };
        }
        let brace_delta = match self.characters[index] {
            '{' => 1,
            '}' => -1,
            _ => 0,
        };
        ExpressionToken { next: index + 1, brace_delta }
    }

    fn comment_end(&self, mut index: usize) -> usize {
        while self.characters.get(index).is_some_and(|character| *character != '\n') {
            index += 1;
        }
        index
    }

    fn skip_whitespace(&self, mut index: usize) -> usize {
        while self.characters.get(index).is_some_and(|character| character.is_whitespace()) {
            index += 1;
        }
        index
    }
}

#[derive(Clone, Copy)]
struct StringLiteral {
    end: usize,
    content_start: usize,
    content_end: usize,
    formatted: bool,
}

struct ExpressionToken {
    next: usize,
    brace_delta: i8,
}

struct FieldParts {
    expression: Range<usize>,
    format_spec: Option<Range<usize>>,
}

enum FieldMarker {
    Conversion,
    FormatSpec,
}

enum GroupClose {
    Complete,
    Mismatch,
    Nested,
}

fn close_group(expected: &mut Vec<char>, character: char) -> GroupClose {
    if expected.pop() != Some(character) {
        GroupClose::Mismatch
    } else if expected.is_empty() {
        GroupClose::Complete
    } else {
        GroupClose::Nested
    }
}

#[derive(Default)]
struct Grouping {
    parentheses: u32,
    brackets: u32,
    braces: u32,
}

impl Grouping {
    const fn is_top_level(&self) -> bool {
        self.parentheses == 0 && self.brackets == 0 && self.braces == 0
    }

    const fn update(&mut self, character: char) {
        match character {
            '(' => self.parentheses += 1,
            ')' => self.parentheses = self.parentheses.saturating_sub(1),
            '[' => self.brackets += 1,
            ']' => self.brackets = self.brackets.saturating_sub(1),
            '{' => self.braces += 1,
            '}' => self.braces = self.braces.saturating_sub(1),
            _ => {}
        }
    }
}

fn dynamic_path(path: &[String]) -> bool {
    if path.iter().any(|component| {
        matches!(
            component.as_str(),
            "breakpoint"
                | "breakpointhook"
                | "__breakpointhook__"
                | "_getframe"
                | "_current_frames"
                | "_FuncBuilder"
                | "currentframe"
                | "exec_module"
                | "f_globals"
                | "import_module"
                | "load_module"
                | "meta_path"
                | "path_hooks"
                | "path_importer_cache"
                | "read_module"
                | "__globals__"
                | "__import__"
                | "__code__"
                | "__subclasses__"
                | "ag_code"
                | "cr_code"
                | "f_code"
                | "gi_code"
        )
    }) {
        return true;
    }
    match path {
        [name, ..]
            if matches!(
                name.as_str(),
                "builtins" | "compile" | "eval" | "exec" | "import_module" | "importlib" | "optparse" | "pkgutil" | "runpy" | "__builtins__" | "__import__"
            ) =>
        {
            true
        }
        [module, submodule, ..] if matches!((module.as_str(), submodule.as_str()), ("logging", "config") | ("unittest", "mock")) => true,
        [module, name, ..] if matches!(module.as_str(), "pickle" | "_pickle") => matches!(name.as_str(), "Unpickler" | "load" | "loads"),
        [module, name, ..] if module == "marshal" => name == "loads",
        [module, name, ..] if module == "types" => matches!(name.as_str(), "CodeType" | "FunctionType"),
        [module, name, ..] if module == "gc" => matches!(name.as_str(), "get_objects" | "get_referrers"),
        _ => false,
    }
}

fn is_identifier_start(character: char) -> bool {
    character == '_' || character.is_alphabetic()
}

fn is_identifier_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::{formatted_code_expressions, has_dynamic_code};

    #[test]
    fn executable_code_construction_is_rejected_without_matching_inert_text() {
        assert_eq!(
            formatted_code_expressions(r#"{os.system("sh quality/hidden.txt")}"#),
            Some(vec![r#"os.system("sh quality/hidden.txt")"#.to_owned()])
        );
        assert!(has_dynamic_code("exec(bytes.fromhex(payload))"));
        assert!(has_dynamic_code("runner = eval\nrunner(source)"));
        assert!(has_dynamic_code("breakpoint()"));
        assert!(has_dynamic_code("sys.breakpointhook()"));
        assert!(has_dynamic_code("sys.__breakpointhook__()"));
        assert!(has_dynamic_code("builtins.compile(source, name, 'exec')"));
        assert!(has_dynamic_code("builtins.__dict__['exec'](payload)"));
        assert!(has_dynamic_code("getattr(builtins, name)(source)"));
        assert!(has_dynamic_code("__import__('os').system(payload)"));
        assert!(has_dynamic_code("importlib.import_module(name)"));
        assert!(has_dynamic_code("importlib.machinery.SourceFileLoader('m', path).load_module()"));
        assert!(has_dynamic_code("from importlib import import_module as load"));
        assert!(has_dynamic_code("from ｉｍｐｏｒｔｌｉｂ import import_module as load"));
        assert!(has_dynamic_code("runpy.run_path('quality/lint.txt')"));
        assert!(has_dynamic_code("sys._getframe().f_globals['os'].system(payload)"));
        assert!(has_dynamic_code("function.__globals__['os'].system(payload)"));
        assert!(has_dynamic_code("object.__subclasses__()"));
        assert!(has_dynamic_code("runpy.run_module(module_name)"));
        assert!(has_dynamic_code("from runpy import run_path as execute"));
        assert!(has_dynamic_code("pkgutil.resolve_name('webbrowser:BackgroundBrowser')"));
        assert!(has_dynamic_code("print.__self__.__import__('webbrowser')"));
        assert!(has_dynamic_code(
            "spec = next(spec for finder in sys.meta_path if (spec := finder.find_spec('webbrowser')) is not None)\nspec.loader.load_module('webbrowser')"
        ));
        assert!(has_dynamic_code("optparse.Values().read_module('webbrowser')"));
        assert!(has_dynamic_code("f'{exec(payload)}'"));
        assert!(has_dynamic_code("f'{ｅｘｅｃ(payload)}'"));
        assert!(has_dynamic_code("f'{ｂｕｉｌｔｉｎｓ.ｅｘｅｃ(payload)}'"));
        assert!(has_dynamic_code("f'{ｉｍｐｏｒｔｌｉｂ.import_module(name)}'"));
        assert!(has_dynamic_code("f\"{_＿import＿_('os')}\""));
        assert!(has_dynamic_code("f'{object._＿subclasses＿_()}'"));
        assert!(has_dynamic_code("f'{value:{exec(payload)}}'"));
        assert!(has_dynamic_code("f'{value:{{exec(payload)}}}'"));
        assert!(has_dynamic_code(r#"f"{exec("payload")}""#));
        assert!(has_dynamic_code("f'''{(\n# an inert } brace\nexec(payload)\n)}'''"));
        assert!(has_dynamic_code("types.FunctionType(marshal.loads(payload), globals())"));
        assert!(has_dynamic_code("pickle.loads(payload)"));
        assert!(has_dynamic_code("_pickle.load(stream)"));
        assert!(has_dynamic_code("pickle.Unpickler(stream).load()"));
        assert!(has_dynamic_code("_pickle.Unpickler(stream).load()"));
        assert!(has_dynamic_code("code_type = (lambda: None).__code__.__class__\ncode_type(*arguments)"));
        assert!(has_dynamic_code("function_type = (lambda: None).__class__\nfunction_type(code, globals())"));
        assert!(has_dynamic_code("function_type = ((lambda value: value)).__class__"));
        assert!(has_dynamic_code("generator.gi_code.__class__(*arguments)"));
        assert!(has_dynamic_code("coroutine.cr_code.__class__(*arguments)"));
        assert!(has_dynamic_code("async_generator.ag_code.__class__(*arguments)"));
        assert!(has_dynamic_code("frame.f_code.__class__(*arguments)"));
        assert!(has_dynamic_code("f'{(lambda: None).__code__.__class__(*arguments)}'"));
        assert!(!has_dynamic_code("pattern = re.compile(r'exec\\(')"));
        assert!(!has_dynamic_code("f'exec is a built-in: {name}'"));
        assert!(!has_dynamic_code("f'{{exec}}: {name}'"));
        assert!(!has_dynamic_code("f'{{exec(payload)}}'"));
        assert!(!has_dynamic_code("# exec(payload)\nprint('eval(source)')"));
        assert!(!has_dynamic_code("# ｅｘｅｃ(payload)\nprint('ｂｕｉｌｔｉｎｓ.ｅｘｅｃ(payload)')"));
        assert!(!has_dynamic_code("f'ｅｘｅｃ(payload) is inert text'"));
        assert!(!has_dynamic_code("f\"{label + 'ｅｘｅｃ(payload)'}\""));
        assert!(!has_dynamic_code("f'{value:─^9}'"));
        assert!(!has_dynamic_code("f\"{value:exec(payload)}\""));
        assert!(!has_dynamic_code(r#"f"{len("safe")}""#));
        assert!(!has_dynamic_code("Exec(payload)\nBuiltins.exec(payload)"));
        assert!(!has_dynamic_code("print('runpy.run_path(payload)')"));
        assert!(!has_dynamic_code("ast.literal_eval(source)"));
        assert!(!has_dynamic_code("print(value.__class__.__name__)"));
        assert!(!has_dynamic_code("transform = lambda value: value.__class__.__name__"));
        assert!(!has_dynamic_code("print('__code__ gi_code cr_code ag_code f_code')"));
    }
}
