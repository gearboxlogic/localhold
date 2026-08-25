pub(super) fn dispatch_is_opaque(arguments: &[String]) -> bool {
    let mut read_only = false;
    let mut archive_seen = false;
    let mut index = 0;
    while let Some(argument) = arguments.get(index).filter(|argument| argument.as_str() != "--") {
        if archive_seen {
            if argument.starts_with('-') {
                return true;
            }
            index += 1;
            continue;
        }
        if matches!(argument.as_str(), "--help" | "--version") {
            read_only = true;
            index += 1;
            continue;
        }
        let Some(options) = argument.strip_prefix('-').filter(|options| !options.starts_with('-')) else {
            archive_seen = true;
            index += 1;
            continue;
        };
        let (flags, password) = options.split_once('P').map_or((options, None), |(flags, password)| (flags, Some(password)));
        if flags.contains(['d', 'x']) || flags.contains('T') {
            return true;
        }
        if password == Some("") {
            index += 1;
            if arguments.get(index).is_none() {
                return true;
            }
        }
        read_only |= flags.chars().any(|option| matches!(option, 'c' | 'h' | 'l' | 'p' | 't' | 'v' | 'z' | 'Z'));
        index += 1;
    }
    !read_only
}
