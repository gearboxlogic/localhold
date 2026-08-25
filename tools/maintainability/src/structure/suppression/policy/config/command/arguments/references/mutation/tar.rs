use super::{DispatchContext, destination_is_opaque, inspection_only};

#[derive(Clone, Copy, Eq, PartialEq)]
enum Operation {
    MutateArchive,
    Extract,
    ReadOnly,
}

struct ParsedArchive<'a> {
    operation: Operation,
    archive: Option<&'a str>,
}

pub(super) fn dispatch_is_opaque(context: DispatchContext<'_>, arguments: &[String]) -> bool {
    if inspection_only("tar", arguments) {
        return false;
    }
    if arguments.iter().any(|argument| argument == "--remove-files") || secondary_output_is_opaque(arguments) {
        return true;
    }
    let Some(parsed) = parse_operation_and_archive(arguments) else {
        return true;
    };
    match parsed.operation {
        Operation::Extract => true,
        Operation::ReadOnly => false,
        Operation::MutateArchive => parsed.archive.is_none_or(|archive| destination_is_opaque(context, archive)),
    }
}

fn secondary_output_is_opaque(arguments: &[String]) -> bool {
    arguments.iter().take_while(|argument| argument.as_str() != "--").enumerate().any(|(index, argument)| {
        if let Some(option) = argument.strip_prefix("--").map(|option| option.split_once('=').map_or(option, |(name, _)| name)) {
            return long_matches(option, "index-file", "index") || long_matches(option, "listed-incremental", "listed");
        }
        let options = argument
            .strip_prefix('-')
            .filter(|options| !options.starts_with('-'))
            .or_else(|| (index == 0 && argument.bytes().all(|byte| byte.is_ascii_alphabetic())).then_some(argument.as_str()));
        options.is_some_and(|options| {
            options
                .chars()
                .take_while(|option| !matches!(option, 'b' | 'C' | 'f' | 'F' | 'I' | 'K' | 'L' | 'N' | 'T' | 'V' | 'X'))
                .any(|option| option == 'g')
        })
    })
}

fn parse_operation_and_archive(arguments: &[String]) -> Option<ParsedArchive<'_>> {
    let mut operation = None;
    let mut archive = None;
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        if index == 0 && !argument.starts_with('-') && argument.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            if argument.chars().any(old_style_option_requires_operand_other_than_archive) {
                return None;
            }
            parse_short_options(argument, &mut operation, &mut archive, arguments, &mut index)?;
        } else if let Some(options) = argument.strip_prefix('-').filter(|options| !options.starts_with('-')) {
            parse_short_options(options, &mut operation, &mut archive, arguments, &mut index)?;
        } else if let Some(option) = argument.strip_prefix("--") {
            let (name, attached) = option.split_once('=').map_or((option, None), |(name, value)| (name, Some(value)));
            if long_matches(name, "extract", "ext") || long_matches(name, "get", "ge") {
                select_operation(&mut operation, Operation::Extract)?;
            } else if long_matches(name, "create", "cre")
                || long_matches(name, "append", "ap")
                || long_matches(name, "update", "u")
                || long_matches(name, "concatenate", "con")
                || long_matches(name, "catenate", "cat")
                || name == "delete"
            {
                select_operation(&mut operation, Operation::MutateArchive)?;
            } else if long_matches(name, "list", "lis") || long_matches(name, "compare", "com") || long_matches(name, "diff", "dif") {
                select_operation(&mut operation, Operation::ReadOnly)?;
            }
            if name == "file" {
                let target = option_value(attached, arguments, &mut index)?;
                set_archive(&mut archive, target)?;
            }
        }
        index += 1;
    }
    Some(ParsedArchive { operation: operation?, archive })
}

const fn old_style_option_requires_operand_other_than_archive(option: char) -> bool {
    matches!(option, 'b' | 'C' | 'F' | 'g' | 'H' | 'I' | 'K' | 'L' | 'N' | 'T' | 'V' | 'X')
}

fn parse_short_options<'a>(options: &'a str, operation: &mut Option<Operation>, archive: &mut Option<&'a str>, arguments: &'a [String], index: &mut usize) -> Option<()> {
    let mut characters = options.char_indices().peekable();
    while let Some((_, option)) = characters.next() {
        match option {
            'c' | 'r' | 'u' | 'A' => select_operation(operation, Operation::MutateArchive)?,
            'x' => select_operation(operation, Operation::Extract)?,
            't' | 'd' => select_operation(operation, Operation::ReadOnly)?,
            'f' => {
                let attached = characters.peek().map(|(offset, _)| &options[*offset..]);
                let target = option_value(attached, arguments, index)?;
                set_archive(archive, target)?;
                return Some(());
            }
            _ => {}
        }
    }
    Some(())
}

fn select_operation(selected: &mut Option<Operation>, operation: Operation) -> Option<()> {
    if selected.is_some_and(|selected| selected != operation) {
        return None;
    }
    *selected = Some(operation);
    Some(())
}

fn option_value<'a>(attached: Option<&'a str>, arguments: &'a [String], index: &mut usize) -> Option<&'a str> {
    if let Some(value) = attached {
        return Some(value);
    }
    *index += 1;
    arguments.get(*index).map(String::as_str)
}

const fn set_archive<'a>(archive: &mut Option<&'a str>, target: &'a str) -> Option<()> {
    if archive.replace(target).is_some() || target.is_empty() {
        return None;
    }
    Some(())
}

fn long_matches(option: &str, full: &str, minimum: &str) -> bool {
    option.len() >= minimum.len() && full.starts_with(option)
}
