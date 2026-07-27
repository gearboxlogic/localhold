use syn::Attribute;
use syn::ext::IdentExt as _;
use syn::spanned::Spanned as _;
use syn::visit::{self, Visit};

const NON_RUST_FENCE_LANGUAGES: &[&str] = &["text"];

pub(super) fn unsupported_runnable_doctest(syntax: &syn::File) -> Option<&'static str> {
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

pub(super) fn is_doc_comment(attribute: &Attribute) -> bool {
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
    fence: Option<char>,
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
        let trimmed = content.trim_start();
        match fence_marker(trimmed) {
            Some(marker) => update_fence(&mut self.fence, marker, trimmed),
            None => self.fence.is_none() && content.starts_with("    ") && !content.trim().is_empty(),
        }
    }
}

fn fence_marker(content: &str) -> Option<char> {
    if content.starts_with("```") {
        Some('`')
    } else if content.starts_with("~~~") {
        Some('~')
    } else {
        None
    }
}

fn update_fence(fence: &mut Option<char>, marker: char, content: &str) -> bool {
    if *fence == Some(marker) {
        *fence = None;
        return false;
    }
    if fence.is_some() {
        return false;
    }
    *fence = Some(marker);
    rustdoc_compiles(content.trim_start_matches(marker).trim())
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
        return tokens
            .iter()
            .any(|token| *token != "custom" && !is_class(token) && !NON_RUST_FENCE_LANGUAGES.contains(token));
    }
    if tokens.iter().any(|token| token.starts_with("ignore-")) {
        return true;
    }
    if tokens.contains(&"ignore") {
        return false;
    }
    let languages: Vec<_> = tokens.iter().filter(|token| !is_class(token)).collect();
    languages.len() != 1 || !NON_RUST_FENCE_LANGUAGES.contains(languages[0])
}
