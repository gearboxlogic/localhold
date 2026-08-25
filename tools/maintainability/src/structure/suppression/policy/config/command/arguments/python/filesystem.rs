use std::collections::BTreeSet;
use std::path::Path;

mod archive;
mod call_arguments;
mod capability;
mod imports;
mod literal;
mod reflection;
mod reviewed;
mod temporary;
mod write_path;

pub(super) fn is_reviewed_dynamic_write_surface(path: &str, source: &str) -> bool {
    reviewed::is_reviewed_dynamic_write_surface(path, source)
}

pub(super) fn has_opaque_write(source: &str) -> bool {
    has_opaque_write_with_policy(source, write_path::Policy::default())
}

pub(super) fn has_opaque_write_in_workspace(workspace: &Path, execution_surfaces: &[String], tracked_paths: &BTreeSet<String>, source: &str) -> bool {
    has_opaque_write_with_policy(source, write_path::Policy::for_workspace(workspace, execution_surfaces, tracked_paths))
}

fn has_opaque_write_with_policy(source: &str, write_policy: write_path::Policy) -> bool {
    if super::evaluation::has_non_ascii_code(source) {
        return true;
    }
    let Some(canonicalized) = imports::canonicalize(source) else {
        return true;
    };
    if reflection::has_opaque_capability(source, &canonicalized.source, &canonicalized.aliases) {
        return true;
    }
    if super::mutates_process_working_directory(&canonicalized.source)
        && has_opaque_canonical_write(&canonicalized.source, canonicalized.aliases.clone(), write_path::Policy::rejecting_all_writes())
    {
        return true;
    }
    has_opaque_canonical_write(&canonicalized.source, canonicalized.aliases, write_policy)
}

fn has_opaque_write_with_aliases(source: &str, aliases: &imports::Aliases, write_policy: &write_path::Policy) -> bool {
    if super::evaluation::has_non_ascii_code(source) {
        return true;
    }
    aliases
        .canonicalize_expression(source)
        .is_none_or(|source| has_opaque_canonical_write(&source, aliases.clone(), write_policy.clone()))
}

fn has_opaque_canonical_write(source: &str, aliases: imports::Aliases, write_policy: write_path::Policy) -> bool {
    let mut scanner = CallScanner::with_aliases(source, aliases, write_policy);
    let mut handled_methods = BTreeSet::new();
    let mut invoked_capabilities = BTreeSet::new();
    while let Some(call) = scanner.next() {
        invoked_capabilities.insert(call.start);
        if handled_methods.contains(&call.start) {
            continue;
        }
        let chained_method = scanner.following_method(&call);
        if modeled_call_is_malformed(&scanner, &call)
            || chained_method
                .as_ref()
                .is_some_and(|method| is_path_mutation_method(called_name(&method.name)) && scanner.call_is_malformed(method.opening_parenthesis))
        {
            return true;
        }
        if let Some(method) = &chained_method
            && is_path_mutation_method(called_name(&method.name))
        {
            handled_methods.insert(method.start);
            invoked_capabilities.insert(method.start);
        }
        if opaque_descriptor_write(called_name(&call.name))
            || chained_method.as_ref().is_some_and(|method| chained_path_write_is_opaque(&scanner, &call, method))
            || direct_path_method_is_opaque(&scanner, &call)
            || direct_open_is_opaque(&scanner, &call)
            || descriptor_open_is_opaque(&scanner, &call)
            || temporary::named_temporary_file_is_opaque(&scanner, &call)
            || mutation_arguments_are_opaque(&scanner, &call)
        {
            return true;
        }
    }
    scanner.opaque_formatted_write_expression || capability::has_opaque_reference(&scanner, &invoked_capabilities)
}

fn modeled_call_is_malformed(scanner: &CallScanner, call: &Call) -> bool {
    let name = called_name(&call.name);
    let method = name.rsplit('.').next().unwrap_or(name);
    scanner.call_is_malformed(call.opening_parenthesis) && (is_direct_mutator(name) || is_path_mutation_method(method))
}

fn called_name(mut name: &str) -> &str {
    while let Some(callable) = name.strip_suffix(".__call__") {
        name = callable;
    }
    name
}

fn is_path_constructor(name: &str) -> bool {
    matches!(name, "Path" | "PosixPath" | "WindowsPath" | "pathlib.Path" | "pathlib.PosixPath" | "pathlib.WindowsPath")
}

fn opaque_descriptor_write(name: &str) -> bool {
    matches!(
        name,
        "os.copy_file_range" | "os.fchmod" | "os.fchown" | "os.ftruncate" | "os.pwrite" | "os.pwritev" | "os.sendfile" | "os.write" | "os.writev"
    )
}

fn is_direct_mutator(name: &str) -> bool {
    archive::is_mutator(name)
        || opaque_descriptor_write(name)
        || matches!(
            name,
            "builtins.open"
                | "codecs.open"
                | "fileinput.FileInput"
                | "fileinput.input"
                | "io.open"
                | "open"
                | "os.fdopen"
                | "os.open"
                | "os.chmod"
                | "os.chown"
                | "os.lchown"
                | "os.link"
                | "os.makedirs"
                | "os.mkdir"
                | "os.remove"
                | "os.removedirs"
                | "os.rename"
                | "os.renames"
                | "os.replace"
                | "os.rmdir"
                | "os.symlink"
                | "os.truncate"
                | "os.unlink"
                | "os.utime"
                | "shutil.copy"
                | "shutil.copy2"
                | "shutil.copyfile"
                | "shutil.copytree"
                | "shutil.move"
                | "shutil.rmtree"
                | "shutil.unpack_archive"
                | "NamedTemporaryFile"
                | "tempfile.NamedTemporaryFile"
        )
}

fn mutation_arguments_are_opaque(scanner: &CallScanner, call: &Call) -> bool {
    let name = called_name(&call.name);
    if archive::mutation_arguments_are_opaque(scanner, call) {
        return true;
    }
    // A tree copy or archive extraction can materialize arbitrarily named
    // protected inputs below an otherwise safe destination. Proving the complete
    // tree stable would require modeling earlier mutations, so this fails closed.
    if matches!(name, "shutil.copytree" | "shutil.unpack_archive") {
        return true;
    }
    if matches!(name, "fileinput.FileInput" | "fileinput.input") {
        return fileinput_inplace_is_opaque(scanner, call);
    }
    let arguments: &[ArgumentSpec] = match name {
        "shutil.copy" | "shutil.copy2" => return directory_destination_is_opaque(scanner, call, false),
        "shutil.move" => return directory_destination_is_opaque(scanner, call, true),
        "shutil.copyfile" => {
            if !follows_symlinks(scanner, call.opening_parenthesis) {
                return true;
            }
            &[ArgumentSpec::new(1, &["dst"])]
        }
        "os.link" | "os.rename" | "os.renames" | "os.replace" => &[ArgumentSpec::new(0, &["src"]), ArgumentSpec::new(1, &["dst"])],
        "os.chmod" | "os.chown" | "os.lchown" | "os.makedirs" | "os.mkdir" | "os.remove" | "os.removedirs" | "os.rmdir" | "os.truncate" | "os.unlink" | "os.utime"
        | "shutil.rmtree" => &[ArgumentSpec::new(0, &["path", "name"])],
        "os.symlink" => return true,
        _ => return false,
    };
    directory_rebase_is_opaque(scanner, call.opening_parenthesis) || arguments.iter().any(|argument| path_argument_is_opaque(scanner, call.opening_parenthesis, *argument))
}

fn fileinput_inplace_is_opaque(scanner: &CallScanner, call: &Call) -> bool {
    if scanner.has_argument_unpack(call.opening_parenthesis) {
        return true;
    }
    let Some(inplace) = scanner.call_argument(call.opening_parenthesis, ArgumentSpec::new(1, &["inplace"])) else {
        return false;
    };
    match inplace.trim() {
        "False" | "None" | "0" => false,
        _ => path_argument_is_opaque(scanner, call.opening_parenthesis, ArgumentSpec::new(0, &["files"])),
    }
}

fn directory_destination_is_opaque(scanner: &CallScanner, call: &Call, mutates_source: bool) -> bool {
    if directory_rebase_is_opaque(scanner, call.opening_parenthesis) || !follows_symlinks(scanner, call.opening_parenthesis) {
        return true;
    }
    if called_name(&call.name) == "shutil.move" && scanner.call_argument(call.opening_parenthesis, ArgumentSpec::new(2, &["copy_function"])).is_some() {
        return true;
    }
    let source = ArgumentSpec::new(0, &["src"]);
    let destination = ArgumentSpec::new(1, &["dst"]);
    if mutates_source && path_argument_is_opaque(scanner, call.opening_parenthesis, source) {
        return true;
    }
    let Some(source) = literal_path_argument(scanner, call.opening_parenthesis, source) else {
        return true;
    };
    let Some(destination) = literal_path_argument(scanner, call.opening_parenthesis, destination) else {
        return true;
    };
    if scanner.write_policy.is_opaque(&destination) {
        return true;
    }
    implicit_destination(&source, &destination).is_none_or(|candidate| scanner.write_policy.is_opaque(&candidate))
}

fn literal_path_argument(scanner: &CallScanner, opening_parenthesis: usize, argument: ArgumentSpec) -> Option<String> {
    scanner.call_argument(opening_parenthesis, argument).and_then(|value| literal_value(&value))
}

fn implicit_destination(source: &str, destination: &str) -> Option<String> {
    let source = source.replace('\\', "/");
    let basename = Path::new(&source).file_name()?.to_str()?;
    let destination = destination.replace('\\', "/");
    Some(Path::new(&destination).join(basename).to_string_lossy().replace('\\', "/"))
}

fn follows_symlinks(scanner: &CallScanner, opening_parenthesis: usize) -> bool {
    // `False` can copy a symlink into a currently missing destination, turning a
    // later otherwise-safe path into a write-through path after analysis.
    scanner
        .call_argument(opening_parenthesis, ArgumentSpec::new(usize::MAX, &["follow_symlinks"]))
        .is_none_or(|value| value.trim().trim_matches(['(', ')']).trim() == "True")
}

fn directory_rebase_is_opaque(scanner: &CallScanner, opening_parenthesis: usize) -> bool {
    scanner.has_argument_unpack(opening_parenthesis)
        || [
            ArgumentSpec::new(usize::MAX, &["dir_fd"]),
            ArgumentSpec::new(usize::MAX, &["src_dir_fd"]),
            ArgumentSpec::new(usize::MAX, &["dst_dir_fd"]),
        ]
        .iter()
        .any(|argument| scanner.call_argument(opening_parenthesis, *argument).is_some_and(|value| value.trim() != "None"))
}

fn chained_path_write_is_opaque(scanner: &CallScanner, call: &Call, method: &Call) -> bool {
    let receiver = ArgumentSpec::new(0, &[]);
    match called_name(&method.name) {
        "chmod" | "lchmod" | "mkdir" | "rmdir" | "touch" | "unlink" | "write_bytes" | "write_text" => path_argument_is_opaque(scanner, call.opening_parenthesis, receiver),
        "open" => {
            scanner.has_argument_unpack(method.opening_parenthesis)
                || writable_mode(scanner, method.opening_parenthesis, 0) && path_argument_is_opaque(scanner, call.opening_parenthesis, receiver)
        }
        "rename" | "replace" => {
            path_argument_is_opaque(scanner, call.opening_parenthesis, receiver) || path_argument_is_opaque(scanner, method.opening_parenthesis, ArgumentSpec::new(0, &["target"]))
        }
        "copy" | "copy_into" | "extract" | "extractall" | "hardlink_to" | "move" | "move_into" | "symlink_to" => true,
        _ => false,
    }
}

fn direct_open_is_opaque(scanner: &CallScanner, call: &Call) -> bool {
    let name = called_name(&call.name);
    if !matches!(name, "builtins.open" | "codecs.open" | "io.open" | "open") {
        return false;
    }
    if scanner.has_argument_unpack(call.opening_parenthesis) {
        return true;
    }
    if name != "codecs.open" && opener_is_opaque(scanner, call.opening_parenthesis) {
        return true;
    }
    if !writable_mode(scanner, call.opening_parenthesis, 1) {
        return false;
    }
    path_argument_is_opaque(scanner, call.opening_parenthesis, ArgumentSpec::new(0, &["file", "filename"]))
}

fn opener_is_opaque(scanner: &CallScanner, opening_parenthesis: usize) -> bool {
    scanner
        .call_argument(opening_parenthesis, ArgumentSpec::new(7, &["opener"]))
        .is_some_and(|opener| opener.trim() != "None")
}

fn direct_path_method_is_opaque(scanner: &CallScanner, call: &Call) -> bool {
    let name = called_name(&call.name);
    let (owner, method) = name.rsplit_once('.').unwrap_or(("", name));
    if matches!(owner, "builtins" | "codecs" | "io" | "os" | "shutil") || owner.is_empty() && method == "open" && !scanner.is_method_call(call.start) {
        return false;
    }
    let class_method = matches!(owner, "Path" | "PosixPath" | "WindowsPath" | "pathlib.Path" | "pathlib.PosixPath" | "pathlib.WindowsPath");
    if !class_method {
        return match method {
            "chmod" | "copy" | "copy_into" | "extract" | "extractall" | "hardlink_to" | "lchmod" | "mkdir" | "move" | "move_into" | "rename" | "replace" | "rmdir"
            | "symlink_to" | "touch" | "unlink" | "write_bytes" | "write_text" => true,
            "open" => scanner.has_argument_unpack(call.opening_parenthesis) || writable_mode(scanner, call.opening_parenthesis, 0),
            _ => false,
        };
    }
    let receiver = ArgumentSpec::new(0, &["self"]);
    match method {
        "chmod" | "lchmod" | "mkdir" | "rmdir" | "touch" | "unlink" | "write_bytes" | "write_text" => path_argument_is_opaque(scanner, call.opening_parenthesis, receiver),
        "open" => {
            scanner.has_argument_unpack(call.opening_parenthesis)
                || writable_mode(scanner, call.opening_parenthesis, 1) && path_argument_is_opaque(scanner, call.opening_parenthesis, receiver)
        }
        "rename" | "replace" => {
            path_argument_is_opaque(scanner, call.opening_parenthesis, receiver) || path_argument_is_opaque(scanner, call.opening_parenthesis, ArgumentSpec::new(1, &["target"]))
        }
        "copy" | "copy_into" | "hardlink_to" | "move" | "move_into" | "symlink_to" => true,
        _ => false,
    }
}

fn is_path_mutation_method(method: &str) -> bool {
    matches!(
        method,
        "chmod"
            | "copy"
            | "copy_into"
            | "extract"
            | "extractall"
            | "hardlink_to"
            | "lchmod"
            | "mkdir"
            | "move"
            | "move_into"
            | "open"
            | "rename"
            | "replace"
            | "rmdir"
            | "symlink_to"
            | "touch"
            | "unlink"
            | "write_bytes"
            | "write_text"
    )
}

fn descriptor_open_is_opaque(scanner: &CallScanner, call: &Call) -> bool {
    let name = called_name(&call.name);
    if name == "os.fdopen" {
        if scanner.has_argument_unpack(call.opening_parenthesis) {
            return true;
        }
        return writable_mode(scanner, call.opening_parenthesis, 1);
    }
    if name != "os.open" || !writable_os_flags(scanner, call.opening_parenthesis) {
        return false;
    }
    directory_rebase_is_opaque(scanner, call.opening_parenthesis) || path_argument_is_opaque(scanner, call.opening_parenthesis, ArgumentSpec::new(0, &["path"]))
}

fn writable_os_flags(scanner: &CallScanner, opening_parenthesis: usize) -> bool {
    let Some(flags) = scanner.call_argument(opening_parenthesis, ArgumentSpec::new(1, &["flags"])) else {
        return true;
    };
    let flags = flags.chars().filter(|character| !character.is_whitespace()).collect::<String>();
    !matches!(flags.as_str(), "0" | "O_RDONLY" | "os.O_RDONLY")
}

fn writable_mode(scanner: &CallScanner, opening_parenthesis: usize, position: usize) -> bool {
    let Some(mode) = scanner.call_argument(opening_parenthesis, ArgumentSpec::new(position, &["mode"])) else {
        return false;
    };
    literal_value(&mode).is_none_or(|mode| mode.contains(['a', 'w', 'x', '+']))
}

fn path_argument_is_opaque(scanner: &CallScanner, opening_parenthesis: usize, argument: ArgumentSpec) -> bool {
    scanner
        .call_argument(opening_parenthesis, argument)
        .is_none_or(|argument| write_path_expression_is_opaque(scanner, &argument))
}

fn write_path_expression_is_opaque(scanner: &CallScanner, argument: &str) -> bool {
    if let Some(path) = literal_value(argument) {
        return scanner.write_policy.is_opaque(&path);
    }
    true
}

#[derive(Clone, Copy)]
struct ArgumentSpec {
    position: usize,
    names: &'static [&'static str],
}

impl ArgumentSpec {
    const fn new(position: usize, names: &'static [&'static str]) -> Self {
        Self { position, names }
    }
}

struct Call {
    start: usize,
    name: String,
    opening_parenthesis: usize,
}

struct MethodReference {
    start: usize,
    name: String,
    end: usize,
}

struct CallScanner {
    characters: Vec<char>,
    index: usize,
    opaque_formatted_write_expression: bool,
    aliases: imports::Aliases,
    write_policy: write_path::Policy,
}

impl CallScanner {
    fn new(source: &str) -> Self {
        Self::with_aliases(source, imports::Aliases::default(), write_path::Policy::default())
    }

    fn with_aliases(source: &str, aliases: imports::Aliases, write_policy: write_path::Policy) -> Self {
        Self {
            characters: source.chars().collect(),
            index: 0,
            opaque_formatted_write_expression: false,
            aliases,
            write_policy,
        }
    }

    fn next(&mut self) -> Option<Call> {
        while self.index < self.characters.len() {
            if self.characters[self.index] == '#' {
                self.skip_comment();
                continue;
            }
            if let Some(literal) = self.string_literal(self.index) {
                self.opaque_formatted_write_expression |= literal.formatted && self.formatted_string_has_opaque_write(&literal);
                self.index = literal.end;
                continue;
            }
            if !is_identifier_start(self.characters[self.index]) {
                self.index += 1;
                continue;
            }
            let start = self.index;
            self.index += 1;
            while self
                .characters
                .get(self.index)
                .is_some_and(|character| is_identifier_character(*character) || *character == '.')
            {
                self.index += 1;
            }
            let name = self.characters[start..self.index].iter().collect::<String>();
            let direct_opening = self.skip_whitespace(self.index);
            let opening_parenthesis = if self.characters.get(direct_opening) == Some(&'(') {
                Some(direct_opening)
            } else {
                self.grouped_identifier_invocation(start, self.index)
            };
            if let Some(opening_parenthesis) = opening_parenthesis {
                self.index = opening_parenthesis + 1;
                return Some(Call { start, name, opening_parenthesis });
            }
        }
        None
    }

    fn skip_comment(&mut self) {
        while self.characters.get(self.index).is_some_and(|character| *character != '\n') {
            self.index += 1;
        }
    }

    fn following_method(&self, call: &Call) -> Option<Call> {
        let method = self.following_method_reference(call)?;
        let direct_opening = self.skip_whitespace(method.end);
        let opening_parenthesis = if self.characters.get(direct_opening) == Some(&'(') {
            Some(direct_opening)
        } else {
            self.grouped_method_invocation(call.start, method.end)
        }?;
        Some(Call {
            start: method.start,
            name: method.name,
            opening_parenthesis,
        })
    }

    fn following_method_reference(&self, call: &Call) -> Option<MethodReference> {
        let mut index = self.closing_parenthesis(call.opening_parenthesis)? + 1;
        index = self.skip_whitespace(index);
        if self.characters.get(index) != Some(&'.') {
            return None;
        }
        index += 1;
        let start = index;
        loop {
            while self.characters.get(index).is_some_and(|character| is_identifier_character(*character)) {
                index += 1;
            }
            let dot = self.skip_whitespace(index);
            if self.characters.get(dot) != Some(&'.') {
                break;
            }
            let next = self.skip_whitespace(dot + 1);
            if !self.characters.get(next).is_some_and(|character| is_identifier_start(*character)) {
                break;
            }
            index = next;
        }
        if index == start {
            return None;
        }
        let name = self.characters[start..index].iter().filter(|character| !character.is_whitespace()).collect::<String>();
        Some(MethodReference { start, name, end: index })
    }

    fn grouped_identifier_invocation(&self, start: usize, end: usize) -> Option<usize> {
        let grouping = self.previous_non_whitespace(start)?;
        if self.characters[grouping] != '(' || !self.is_grouping_parenthesis(grouping) {
            return None;
        }
        let closing = self.skip_whitespace(end);
        if self.characters.get(closing) != Some(&')') || self.closing_parenthesis(grouping) != Some(closing) {
            return None;
        }
        let invocation = self.skip_whitespace(closing + 1);
        (self.characters.get(invocation) == Some(&'(')).then_some(invocation)
    }

    fn grouped_method_invocation(&self, receiver_start: usize, method_end: usize) -> Option<usize> {
        let grouping = self.previous_non_whitespace(receiver_start)?;
        if self.characters[grouping] != '(' || !self.is_grouping_parenthesis(grouping) {
            return None;
        }
        let closing = self.skip_whitespace(method_end);
        if self.characters.get(closing) != Some(&')') || self.closing_parenthesis(grouping) != Some(closing) {
            return None;
        }
        let invocation = self.skip_whitespace(closing + 1);
        (self.characters.get(invocation) == Some(&'(')).then_some(invocation)
    }

    fn previous_non_whitespace(&self, start: usize) -> Option<usize> {
        self.characters[..start].iter().rposition(|character| !character.is_whitespace())
    }

    fn is_grouping_parenthesis(&self, opening: usize) -> bool {
        !opening
            .checked_sub(1)
            .and_then(|index| self.characters.get(index))
            .is_some_and(|character| is_identifier_character(*character) || matches!(character, ')' | ']' | '}'))
    }

    fn string_end(&self, start: usize) -> Option<usize> {
        self.string_literal(start).map(|literal| literal.end)
    }

    fn string_literal(&self, start: usize) -> Option<StringLiteral> {
        if start > 0 && is_identifier_character(self.characters[start - 1]) {
            return None;
        }
        let mut quote = start;
        let mut formatted = false;
        while quote < self.characters.len() && quote - start < 3 && matches!(self.characters[quote].to_ascii_lowercase(), 'b' | 'f' | 'r' | 'u') {
            formatted |= self.characters[quote].eq_ignore_ascii_case(&'f');
            quote += 1;
        }
        let delimiter = *self.characters.get(quote).filter(|character| matches!(character, '\'' | '"'))?;
        let triple = self.characters.get(quote + 1) == Some(&delimiter) && self.characters.get(quote + 2) == Some(&delimiter);
        let width = if triple { 3 } else { 1 };
        let mut index = quote + width;
        while index < self.characters.len() {
            if self.characters[index] == '\\' {
                index = index.saturating_add(2);
            } else if formatted && self.characters[index] == '{' {
                index = self.formatted_content_end(index);
            } else if self.characters[index] == delimiter && (!triple || self.characters.get(index + 1) == Some(&delimiter) && self.characters.get(index + 2) == Some(&delimiter)) {
                return Some(StringLiteral {
                    end: index + width,
                    content_start: quote + width,
                    content_end: index,
                    formatted,
                });
            } else {
                index += 1;
            }
        }
        None
    }

    fn formatted_content_end(&self, opening: usize) -> usize {
        if self.characters.get(opening + 1) == Some(&'{') {
            return opening + 2;
        }
        self.f_expression_end(opening + 1, self.characters.len())
            .map_or(self.characters.len(), |closing| closing + 1)
    }

    fn formatted_string_has_opaque_write(&self, literal: &StringLiteral) -> bool {
        let content = self.characters[literal.content_start..literal.content_end].iter().collect::<String>();
        super::evaluation::formatted_code_expressions(&content).is_none_or(|expressions| {
            expressions
                .iter()
                .any(|expression| has_opaque_write_with_aliases(expression, &self.aliases, &self.write_policy))
        })
    }

    fn f_expression_end(&self, start: usize, end: usize) -> Option<usize> {
        let mut depth = 1_u32;
        let mut index = start;
        while index < end {
            if self.characters[index] == '#' {
                index = self.comment_end(index);
                continue;
            }
            if let Some(literal) = self.string_literal(index) {
                index = literal.end;
                continue;
            }
            match self.characters[index] {
                '{' => depth += 1,
                '}' if depth == 1 => return Some(index),
                '}' => depth -= 1,
                _ => {}
            }
            index += 1;
        }
        None
    }

    fn comment_end(&self, mut index: usize) -> usize {
        while self.characters.get(index).is_some_and(|character| *character != '\n') {
            index += 1;
        }
        index
    }

    fn skip_whitespace(&self, mut index: usize) -> usize {
        while self.characters.get(index).is_some_and(|character| character.is_whitespace()) {
            index += 1;
        }
        index
    }

    fn skip_trivia(&self, mut index: usize) -> usize {
        loop {
            index = self.skip_whitespace(index);
            if self.characters.get(index) != Some(&'#') {
                return index;
            }
            index = self.comment_end(index);
        }
    }

    fn is_method_call(&self, start: usize) -> bool {
        self.characters[..start].iter().rev().find(|character| !character.is_whitespace()) == Some(&'.')
    }
}

struct StringLiteral {
    end: usize,
    content_start: usize,
    content_end: usize,
    formatted: bool,
}

fn literal_value(argument: &str) -> Option<String> {
    literal::static_string(argument)
}

fn is_identifier_start(character: char) -> bool {
    character == '_' || character.is_alphabetic()
}

fn is_identifier_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

#[cfg(test)]
mod tests;
