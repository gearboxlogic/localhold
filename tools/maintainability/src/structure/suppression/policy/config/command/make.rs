use std::path::Path;

use anyhow::{Result, bail};

pub(super) fn validate_surface(path: &Path, source: &str) -> Result<()> {
    if !is_make_surface(path) {
        return Ok(());
    }
    let display = path.to_string_lossy();
    if has_directive(source, &["include", "-include", "sinclude"]) {
        bail!("checked-in Make command surface {display:?} uses unsupported include indirection");
    }
    if has_directive(source, &["load"]) || has_assignment_operator(source, "!=") || has_make_function(source, &["eval", "file", "guile", "shell"]) {
        bail!("checked-in Make command surface {display:?} uses an unsupported command-producing expansion");
    }
    if changes_recipe_prefix(source) || recipe_uses_expansion(source) {
        bail!("checked-in Make command surface {display:?} uses unsupported dynamic recipe expansion");
    }
    Ok(())
}

fn is_make_surface(path: &Path) -> bool {
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    matches!(basename, "Makefile" | "makefile" | "GNUmakefile")
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mk"))
}

fn has_directive(source: &str, directives: &[&str]) -> bool {
    source.lines().filter_map(make_directive).any(|directive| directives.contains(&directive))
}

fn make_directive(line: &str) -> Option<&str> {
    make_control_line(line)?.split_ascii_whitespace().next()
}

fn make_control_line(line: &str) -> Option<&str> {
    if line.starts_with('\t') {
        return None;
    }
    let line = line.trim_start();
    (!line.starts_with('#')).then_some(line)
}

fn has_assignment_operator(source: &str, operator: &str) -> bool {
    source
        .lines()
        .filter_map(make_control_line)
        .any(|line| line.split_once('#').unwrap_or((line, "")).0.contains(operator))
}

fn has_make_function(source: &str, functions: &[&str]) -> bool {
    source
        .lines()
        .filter(|line| line.starts_with('\t') || make_control_line(line).is_some())
        .any(|line| line_has_make_function(line, functions))
}

fn line_has_make_function(line: &str, functions: &[&str]) -> bool {
    line.match_indices(['(', '{']).any(|(index, _)| {
        let Some(prefix) = line.get(..index) else {
            return false;
        };
        if !prefix.ends_with('$') {
            return false;
        }
        let name = line[index + 1..]
            .trim_start()
            .split(|character: char| character.is_ascii_whitespace() || matches!(character, ',' | ')' | '}'))
            .next()
            .unwrap_or_default();
        functions.contains(&name)
    })
}

fn changes_recipe_prefix(source: &str) -> bool {
    source
        .lines()
        .filter_map(make_control_line)
        .any(|line| line.starts_with(".RECIPEPREFIX") && line[".RECIPEPREFIX".len()..].trim_start().starts_with([':', '?', '+', '=']))
}

fn recipe_uses_expansion(source: &str) -> bool {
    source.lines().any(|line| {
        let recipe = line.strip_prefix('\t').or_else(|| {
            make_control_line(line)
                .and_then(|line| line.split_once(';'))
                .filter(|(rule, _)| rule.contains(':'))
                .map(|(_, recipe)| recipe)
        });
        recipe.is_some_and(|recipe| recipe.contains('$'))
    })
}

#[cfg(test)]
mod tests {
    use super::validate_surface;
    use std::path::Path;

    #[test]
    fn external_and_generated_make_recipes_fail_closed() {
        for source in [
            "lint:\n\t$(shell cat quality/lint.txt)\n",
            "LINT != cat quality/lint.txt\nlint:\n\ttrue\n",
            "load quality/lint.so\n",
            "lint:\n\t$(LINT_COMMAND)\n",
            ".RECIPEPREFIX := >\n",
        ] {
            assert!(validate_surface(Path::new("Makefile"), source).is_err(), "accepted {source:?}");
        }
    }

    #[test]
    fn static_make_recipes_remain_supported() {
        validate_surface(Path::new("Makefile"), "lint:\n\tcargo clippy -- -D warnings\n").expect("static Make recipe");
        validate_surface(Path::new("Makefile"), "# $(shell ignored)\nlint:\n\ttest one != two\n").expect("non-executing Make text");
        validate_surface(Path::new("script/check.sh"), "lint:\n\t$(LINT_COMMAND)\n").expect("non-Make command surface");
    }
}
