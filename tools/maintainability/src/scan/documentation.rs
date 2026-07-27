pub(super) fn unsupported_runnable_doctest(source: &str) -> Option<&'static str> {
    let mut block_doc = false;
    let mut fence = None;
    for line in source.lines() {
        let Some(content) = doc_content(line, &mut block_doc) else {
            continue;
        };
        let content = content.strip_prefix(' ').unwrap_or(&content);
        let trimmed = content.trim_start();
        let marker = if trimmed.starts_with("```") {
            Some('`')
        } else if trimmed.starts_with("~~~") {
            Some('~')
        } else {
            None
        };
        if let Some(marker) = marker {
            if update_fence(&mut fence, marker, trimmed) {
                return Some("runnable Rust doctests are unsupported by the source-safety gate");
            }
        } else if fence.is_none() && content.starts_with("    ") && !content.trim().is_empty() {
            return Some("indented runnable Rust doctests are unsupported by the source-safety gate");
        }
    }
    None
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
        .collect();
    if tokens.contains(&"ignore") {
        return false;
    }
    tokens
        .iter()
        .any(|token| matches!(*token, "rust" | "no_run" | "should_panic" | "compile_fail") || token.starts_with("edition"))
}
