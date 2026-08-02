use std::path::Path;

use super::{is_windows_absolute, path};

pub(super) fn dispatch_is_opaque(path: &str, arguments: &[String]) -> bool {
    let mut options_ended = false;
    arguments
        .iter()
        .take_while(|argument| argument.as_str() != "--help" && argument.as_str() != "--version")
        .any(|argument| {
            if argument == "--" {
                options_ended = true;
                return false;
            }
            if !options_ended && argument.starts_with('-') && argument != "-" {
                return powershell_target(argument).is_some_and(|target| target_is_opaque(path, target));
            }
            target_is_opaque(path, argument)
        })
}

fn powershell_target(argument: &str) -> Option<&str> {
    let (option, target) = argument.split_once(':')?;
    matches!(option.to_ascii_lowercase().as_str(), "-literalpath" | "-path").then_some(target)
}

fn target_is_opaque(path: &str, target: &str) -> bool {
    if is_reviewed_dynamic_target(path, target) {
        return false;
    }
    if let Some(target) = path::normalize_literal(target) {
        return super::super::super::super::is_protected_check_input(&target);
    }
    path::contains_dynamic_value(target) || !(Path::new(target).is_absolute() || is_windows_absolute(target))
}

fn is_reviewed_dynamic_target(path: &str, target: &str) -> bool {
    // The bootstrap authenticates this complete fixture driver before running
    // it, so its isolated cleanup paths are reviewed as a set.
    (path == "script/tests/test_maintainability_bootstrap.sh" && path::contains_dynamic_value(target)) || REVIEWED_DYNAMIC_TARGETS.contains(&(path, target))
}

const REVIEWED_DYNAMIC_TARGETS: &[(&str, &str)] = &[
    ("script/check-maintainability-bootstrap.sh", "$destination"),
    ("script/check-maintainability-bootstrap.sh", "$snapshot_root"),
    ("script/check-maintainability-bootstrap.sh", "$snapshot_root/tools/maintainability/Cargo.toml"),
    ("script/check-maintainability-bootstrap.sh", "$snapshot_root/tools/maintainability/Cargo.lock"),
    ("script/claude-review.sh", "$scratch_directory"),
    ("script/run-maintainability-gate.sh", "$target_directory"),
    ("script/tests/test_claude_review.sh", "$signal_scratch"),
    ("script/tests/test_claude_review.sh", "$test_root"),
    ("script/tests/test_claude_review.sh", "$test_root/capture"),
];
