use super::{DispatchContext, path_policy};

pub(super) fn dispatch_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    let mut options_ended = false;
    for argument in arguments {
        if !options_ended && argument == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && matches!(argument.as_str(), "--help" | "--version") {
            return false;
        }
        if !options_ended && argument.starts_with('-') && argument != "-" {
            if powershell_target(argument).is_some_and(|target| path_policy::target_is_opaque(context.path_policy, target, context.semantics)) {
                return true;
            }
            continue;
        }
        if path_policy::target_is_opaque(context.path_policy, argument, context.semantics) {
            return true;
        }
    }
    false
}

fn powershell_target(argument: &str) -> Option<&str> {
    let (option, target) = argument.split_once(':')?;
    matches!(option.to_ascii_lowercase().as_str(), "-literalpath" | "-path").then_some(target)
}
