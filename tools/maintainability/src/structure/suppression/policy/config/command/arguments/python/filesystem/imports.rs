use std::collections::BTreeMap;
use std::ops::Range;

use super::{CallScanner, is_direct_mutator, is_identifier_character, is_identifier_start, is_path_constructor};

pub(super) struct Canonicalized {
    pub(super) source: String,
    pub(super) aliases: Aliases,
}

pub(super) fn canonicalize(source: &str) -> Option<Canonicalized> {
    let scanner = CallScanner::new(source);
    let statements = statement_ranges(&scanner);
    let mut aliases = BTreeMap::new();
    let mut ignored = vec![false; scanner.characters.len()];
    for range in statements {
        let tokens = tokens(&scanner.characters[range.clone()]);
        let parsed = match tokens.first() {
            Some(Token::Identifier(keyword)) if keyword == "import" => parse_module_import(&tokens[1..]),
            Some(Token::Identifier(keyword)) if keyword == "from" => parse_from_import(&tokens[1..]),
            _ if contains_relevant_inline_import(&tokens) => return None,
            _ => continue,
        }?;
        for (alias, target) in parsed {
            match aliases.get(&alias) {
                Some(existing) if existing != &target => return None,
                Some(_) => {}
                None => {
                    aliases.insert(alias, target);
                }
            }
        }
        ignored[range].fill(true);
    }
    let aliases = Aliases(aliases);
    let source = rewrite_aliases(&scanner, &aliases, &ignored)?;
    Some(Canonicalized { source, aliases })
}

#[derive(Clone, Default)]
pub(super) struct Aliases(BTreeMap<String, Alias>);

impl Aliases {
    pub(super) fn canonicalize_expression(&self, source: &str) -> Option<String> {
        let scanner = CallScanner::new(source);
        let ignored = vec![false; scanner.characters.len()];
        rewrite_aliases(&scanner, self, &ignored)
    }
}

fn statement_ranges(scanner: &CallScanner) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut index = 0;
    let mut depth = 0_u32;
    while index < scanner.characters.len() {
        if let Some(literal) = scanner.string_literal(index) {
            index = literal.end;
            continue;
        }
        if scanner.characters[index] == '#' {
            index = scanner.comment_end(index);
            continue;
        }
        match scanner.characters[index] {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '\n' | ';' if depth == 0 => {
                ranges.push(start..index);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    ranges.push(start..scanner.characters.len());
    ranges
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Alias {
    Module(&'static str),
    Callable(String),
}

type ParsedAliases = Vec<(String, Alias)>;

fn parse_module_import(tokens: &[Token]) -> Option<ParsedAliases> {
    let mut parsed = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let (module, next) = qualified_name(tokens, index)?;
        index = next;
        let explicit_alias = if identifier_at(tokens, index) == Some("as") {
            let alias = identifier_at(tokens, index + 1)?.to_owned();
            index += 2;
            Some(alias)
        } else {
            None
        };
        if let Some(canonical) = canonical_module(&module) {
            let alias = explicit_alias.unwrap_or_else(|| module.split('.').next().unwrap_or(&module).to_owned());
            parsed.push((alias, Alias::Module(canonical)));
        }
        if index == tokens.len() {
            break;
        }
        if tokens.get(index) != Some(&Token::Comma) {
            return None;
        }
        index += 1;
    }
    Some(parsed)
}

fn parse_from_import(tokens: &[Token]) -> Option<ParsedAliases> {
    let (module, mut index) = qualified_name(tokens, 0)?;
    if identifier_at(tokens, index) != Some("import") {
        return None;
    }
    index += 1;
    let parenthesized = tokens.get(index) == Some(&Token::LeftParenthesis);
    if parenthesized {
        index += 1;
    }
    let canonical_module = canonical_module(&module);
    let mut parsed = Vec::new();
    while index < tokens.len() {
        if parenthesized && tokens.get(index) == Some(&Token::RightParenthesis) {
            index += 1;
            break;
        }
        let name = match tokens.get(index) {
            Some(Token::Identifier(name)) => name.clone(),
            Some(Token::Star) if canonical_module.is_some() => return None,
            _ => return None,
        };
        index += 1;
        let alias = if identifier_at(tokens, index) == Some("as") {
            let alias = identifier_at(tokens, index + 1)?.to_owned();
            index += 2;
            alias
        } else {
            name.clone()
        };
        if let Some(module) = canonical_module {
            let target = format!("{module}.{name}");
            if is_direct_mutator(&target) || is_path_constructor(&target) {
                parsed.push((alias, Alias::Callable(target)));
            }
        }
        if parenthesized && tokens.get(index) == Some(&Token::RightParenthesis) {
            index += 1;
            break;
        }
        if index == tokens.len() {
            break;
        }
        if tokens.get(index) != Some(&Token::Comma) {
            return None;
        }
        index += 1;
    }
    (index == tokens.len()).then_some(parsed)
}

fn rewrite_aliases(scanner: &CallScanner, aliases: &Aliases, ignored: &[bool]) -> Option<String> {
    let mut output = String::with_capacity(scanner.characters.len());
    let mut index = 0;
    while index < scanner.characters.len() {
        if ignored[index] {
            output.push(if scanner.characters[index] == '\n' { '\n' } else { ' ' });
            index += 1;
            continue;
        }
        if let Some(literal) = scanner.string_literal(index) {
            output.extend(scanner.characters[index..literal.end].iter());
            index = literal.end;
            continue;
        }
        if scanner.characters[index] == '#' {
            let end = scanner.comment_end(index);
            output.extend(scanner.characters[index..end].iter());
            index = end;
            continue;
        }
        if !is_identifier_start(scanner.characters[index]) {
            output.push(scanner.characters[index]);
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while scanner.characters.get(index).is_some_and(|character| is_identifier_character(*character)) {
            index += 1;
        }
        let name = scanner.characters[start..index].iter().collect::<String>();
        let root_reference = scanner.characters[..start].iter().rfind(|character| !character.is_whitespace()) != Some(&'.');
        let Some(alias) = root_reference.then(|| aliases.0.get(&name)).flatten() else {
            output.push_str(&name);
            continue;
        };
        match alias {
            Alias::Callable(target) => output.push_str(target),
            Alias::Module(target) => {
                let following = scanner.skip_whitespace(index);
                if scanner.characters.get(following) != Some(&'.') {
                    return None;
                }
                output.push_str(target);
            }
        }
    }
    Some(output)
}

fn canonical_module(module: &str) -> Option<&'static str> {
    match module {
        "builtins" => Some("builtins"),
        "_io" | "_pyio" | "io" => Some("io"),
        "nt" | "os" | "posix" => Some("os"),
        "pathlib" => Some("pathlib"),
        "shutil" => Some("shutil"),
        "tempfile" => Some("tempfile"),
        _ => None,
    }
}

fn contains_relevant_inline_import(tokens: &[Token]) -> bool {
    tokens.windows(2).any(|tokens| {
        matches!(&tokens[0], Token::Identifier(keyword) if keyword == "import") && matches!(&tokens[1], Token::Identifier(module) if canonical_module(module).is_some())
    }) || tokens.windows(3).any(|tokens| {
        matches!(&tokens[0], Token::Identifier(keyword) if keyword == "from")
            && matches!(&tokens[1], Token::Identifier(module) if canonical_module(module).is_some())
            && matches!(&tokens[2], Token::Identifier(keyword) if keyword == "import")
    })
}

fn qualified_name(tokens: &[Token], mut index: usize) -> Option<(String, usize)> {
    let mut name = identifier_at(tokens, index)?.to_owned();
    index += 1;
    while tokens.get(index) == Some(&Token::Dot) {
        name.push('.');
        name.push_str(identifier_at(tokens, index + 1)?);
        index += 2;
    }
    Some((name, index))
}

fn identifier_at(tokens: &[Token], index: usize) -> Option<&str> {
    match tokens.get(index) {
        Some(Token::Identifier(identifier)) => Some(identifier),
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Token {
    Identifier(String),
    Dot,
    Comma,
    Star,
    LeftParenthesis,
    RightParenthesis,
    Unsupported,
}

fn tokens(characters: &[char]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        if characters[index] == '#' {
            break;
        }
        if characters[index].is_whitespace() {
            index += 1;
            continue;
        }
        if is_identifier_start(characters[index]) {
            let start = index;
            index += 1;
            while characters.get(index).is_some_and(|character| is_identifier_character(*character)) {
                index += 1;
            }
            tokens.push(Token::Identifier(characters[start..index].iter().collect()));
            continue;
        }
        tokens.push(match characters[index] {
            '.' => Token::Dot,
            ',' => Token::Comma,
            '*' => Token::Star,
            '(' => Token::LeftParenthesis,
            ')' => Token::RightParenthesis,
            _ => Token::Unsupported,
        });
        index += 1;
    }
    tokens
}
