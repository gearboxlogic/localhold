use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

const PYTHON_ANALYZER: &str = "tools/maintainability/src/structure/suppression/policy/config/command/arguments/python";
const MAINTAINABILITY_MANIFEST: &str = "tools/maintainability/Cargo.toml";
const PYTHON_ANALYZER_FILE_LIMIT: usize = 800;

pub(super) fn validate_python_analyzer(workspace: &Path, tracked_paths: &BTreeSet<String>) -> Result<()> {
    if !tracked_paths.contains(MAINTAINABILITY_MANIFEST) {
        return Ok(());
    }
    let coordinator = format!("{PYTHON_ANALYZER}.rs");
    if !tracked_paths.contains(&coordinator) {
        bail!("maintainability Python analyzer coordinator {coordinator:?} must remain tracked");
    }
    for path in tracked_paths.iter().filter(|path| is_python_analyzer_source(path)) {
        let source_path = workspace.join(path);
        let metadata = fs::symlink_metadata(&source_path).with_context(|| format!("inspect maintainability Python analyzer {path:?}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("maintainability Python analyzer source {path:?} must be a regular non-symlink file");
        }
        let source = fs::read_to_string(&source_path).with_context(|| format!("read maintainability Python analyzer {path:?}"))?;
        let physical_lines = physical_line_count(&source);
        if physical_lines > PYTHON_ANALYZER_FILE_LIMIT {
            bail!("maintainability Python analyzer source {path:?} has {physical_lines} physical lines, exceeding its {PYTHON_ANALYZER_FILE_LIMIT}-line limit");
        }
    }
    Ok(())
}

fn is_python_analyzer_source(path: &str) -> bool {
    path == format!("{PYTHON_ANALYZER}.rs")
        || path.starts_with(&format!("{PYTHON_ANALYZER}/")) && Path::new(path).extension().is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

fn physical_line_count(source: &str) -> usize {
    source.bytes().filter(|byte| *byte == b'\n').count() + usize::from(!source.is_empty() && !source.ends_with('\n'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audited_python_analyzer_sources_cannot_exceed_the_production_file_limit() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let coordinator = format!("{PYTHON_ANALYZER}.rs");
        let child = format!("{PYTHON_ANALYZER}/execution.rs");
        for path in [&coordinator, &child] {
            let target = workspace.path().join(path);
            fs::create_dir_all(target.parent().expect("analyzer source parent")).expect("analyzer source directory");
            fs::write(target, "line\n").expect("analyzer source");
        }
        let tracked_paths = BTreeSet::from([MAINTAINABILITY_MANIFEST.to_owned(), coordinator, child.clone()]);
        validate_python_analyzer(workspace.path(), &tracked_paths).expect("analyzer sources within limit");

        fs::write(workspace.path().join(&child), "line\n".repeat(PYTHON_ANALYZER_FILE_LIMIT + 1)).expect("oversized analyzer source");
        let error = validate_python_analyzer(workspace.path(), &tracked_paths).unwrap_err();
        assert!(error.to_string().contains("exceeding its 800-line limit"), "{error:#}");
    }
}
