use syn::Attribute;
use syn::ext::IdentExt as _;
use syn::spanned::Spanned as _;
use syn::visit::{self, Visit};

pub fn unsupported_runnable_doctest(syntax: &syn::File) -> Option<&'static str> {
    let mut collector = DocCommentCollector::default();
    collector.visit_file(syntax);

    let mut scanner = DocCommentScanner::default();
    let mut previous_end = None;
    for comment in collector.comments {
        if previous_end.is_some_and(|end| comment.start_line > end + 1) {
            scanner = DocCommentScanner::default();
        }
        if scanner.scan(&comment.source) {
            return Some("runnable Rust doctests are unsupported by the source-safety gate");
        }
        previous_end = Some(comment.end_line);
    }
    None
}

pub fn is_doc_comment(attribute: &Attribute) -> bool {
    doc_comment_source(attribute).is_some()
}

#[derive(Default)]
struct DocCommentCollector {
    comments: Vec<DocComment>,
}

impl<'ast> Visit<'ast> for DocCommentCollector {
    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        if let Some(source) = doc_comment_source(attribute) {
            let span = attribute.span();
            self.comments.push(DocComment {
                source,
                start_line: span.start().line,
                end_line: span.end().line,
            });
        }
        visit::visit_attribute(self, attribute);
    }
}

struct DocComment {
    source: String,
    start_line: usize,
    end_line: usize,
}

fn doc_comment_source(attribute: &Attribute) -> Option<String> {
    let is_doc = attribute.path().segments.len() == 1 && attribute.path().segments[0].ident.unraw() == "doc";
    let source = is_doc.then(|| attribute.span().source_text()).flatten()?;
    let trimmed = source.trim_start();
    (trimmed.starts_with("///") || trimmed.starts_with("//!") || trimmed.starts_with("/**") || trimmed.starts_with("/*!")).then_some(source)
}

#[derive(Default)]
struct DocCommentScanner {
    block_doc: bool,
    fence: Option<Fence>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Fence {
    marker: char,
    length: usize,
}

impl DocCommentScanner {
    fn scan(&mut self, source: &str) -> bool {
        for line in source.lines() {
            let Some(content) = doc_content(line, &mut self.block_doc) else {
                continue;
            };
            if self.scan_content(&content) {
                return true;
            }
        }
        false
    }

    fn scan_content(&mut self, content: &str) -> bool {
        let content = content.strip_prefix(' ').unwrap_or(content);
        let content = strip_markdown_container_prefixes(content);
        let trimmed = content.trim_start();
        match fence_delimiter(trimmed) {
            Some((delimiter, rest)) => update_fence(&mut self.fence, delimiter, rest),
            None => self.fence.is_none() && is_indented_code(content),
        }
    }
}

fn strip_markdown_container_prefixes(mut content: &str) -> &str {
    loop {
        let candidate = content.trim_start();
        if let Some(rest) = candidate.strip_prefix('>') {
            content = strip_container_separator(rest);
        } else if let Some(rest) = strip_list_marker(candidate) {
            content = rest;
        } else if let Some(rest) = strip_footnote_definition(candidate) {
            content = rest;
        } else {
            return content;
        }
    }
}

fn strip_list_marker(content: &str) -> Option<&str> {
    let bytes = content.as_bytes();
    let marker_length = match bytes {
        [b'-' | b'+' | b'*', whitespace, ..] if whitespace.is_ascii_whitespace() => 1,
        _ => {
            let digits = bytes.iter().take(9).take_while(|byte| byte.is_ascii_digit()).count();
            if digits == 0 || !matches!(bytes.get(digits), Some(b'.' | b')')) || !bytes.get(digits + 1).is_some_and(u8::is_ascii_whitespace) {
                return None;
            }
            digits + 1
        }
    };
    Some(strip_container_separator(&content[marker_length..]))
}

fn strip_footnote_definition(content: &str) -> Option<&str> {
    let rest = content.strip_prefix("[^")?;
    let (label, content) = rest.split_once("]:")?;
    (!label.is_empty() && !label.contains('[') && !label.contains(']')).then(|| strip_container_separator(content))
}

fn strip_container_separator(content: &str) -> &str {
    content.strip_prefix(' ').or_else(|| content.strip_prefix('\t')).unwrap_or(content)
}

fn is_indented_code(content: &str) -> bool {
    let mut columns = 0_usize;
    for character in content.chars() {
        match character {
            ' ' => columns += 1,
            '\t' => columns += 4 - columns % 4,
            _ => return columns >= 4,
        }
    }
    false
}

fn fence_delimiter(content: &str) -> Option<(Fence, &str)> {
    let Some(marker @ ('`' | '~')) = content.chars().next() else {
        return None;
    };
    let length = content.chars().take_while(|character| *character == marker).count();
    (length >= 3).then(|| (Fence { marker, length }, &content[length..]))
}

fn update_fence(fence: &mut Option<Fence>, delimiter: Fence, rest: &str) -> bool {
    if fence.is_some_and(|opening| delimiter.marker == opening.marker && delimiter.length >= opening.length && rest.trim().is_empty()) {
        *fence = None;
        return false;
    }
    if fence.is_some() {
        return false;
    }
    *fence = Some(delimiter);
    rustdoc_compiles(rest.trim())
}

fn doc_content(line: &str, block_doc: &mut bool) -> Option<String> {
    let trimmed = line.trim_start();
    if *block_doc {
        let (content, ended) = trimmed.split_once("*/").map_or((trimmed, false), |(content, _)| (content, true));
        *block_doc = !ended;
        return Some(content.strip_prefix('*').unwrap_or(content).to_owned());
    }
    if let Some(content) = trimmed.strip_prefix("///")
        && !trimmed.starts_with("////")
    {
        return Some(content.to_owned());
    }
    if let Some(content) = trimmed.strip_prefix("//!") {
        return Some(content.to_owned());
    }
    let content = trimmed.strip_prefix("/**").or_else(|| trimmed.strip_prefix("/*!"))?;
    let (content, ended) = content.split_once("*/").map_or((content, false), |(content, _)| (content, true));
    *block_doc = !ended;
    Some(content.to_owned())
}

fn rustdoc_compiles(info: &str) -> bool {
    if info.is_empty() {
        return true;
    }
    let tokens: Vec<_> = info
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|token| !token.is_empty())
        .map(|token| token.trim_matches(['{', '}']))
        .collect();
    let is_class = |token: &&str| token.starts_with('.') || token.starts_with("class=");
    if tokens.contains(&"custom") {
        return false;
    }
    if tokens.iter().any(|token| token.starts_with("ignore-")) {
        return true;
    }
    if tokens.contains(&"ignore") {
        return false;
    }
    tokens.iter().any(is_class) || tokens.iter().any(rustdoc_rust_modifier)
}

fn rustdoc_rust_modifier(token: &&str) -> bool {
    matches!(*token, "rust" | "no_run" | "should_panic" | "compile_fail" | "standalone_crate" | "test_harness") || token.starts_with("edition") || token.starts_with("ignore-")
}
