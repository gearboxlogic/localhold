use std::path::Path;

use super::{ArgumentSpec, Call, CallScanner, called_name, literal_path_argument, path_argument_is_opaque, writable_mode, write_path_expression_is_opaque};

pub(super) fn is_mutator(name: &str) -> bool {
    matches!(
        name,
        "shutil.make_archive"
            | "zipapp.create_archive"
            | "zipapp.main"
            | "zipfile.main"
            | "zipfile.PyZipFile"
            | "zipfile.ZipFile"
            | "zipfile._path"
            | "zipfile._path.CompleteDirs"
            | "zipfile._path.FastLookup"
    )
}

pub(super) fn mutation_arguments_are_opaque(scanner: &CallScanner, call: &Call) -> bool {
    match called_name(&call.name) {
        "shutil.make_archive" | "zipapp.main" | "zipfile.main" | "zipfile._path" => true,
        "zipapp.create_archive" => zipapp_target_is_opaque(scanner, call),
        "zipfile.PyZipFile" | "zipfile.ZipFile" | "zipfile._path.CompleteDirs" | "zipfile._path.FastLookup" => zip_target_is_opaque(scanner, call),
        _ => false,
    }
}

fn zip_target_is_opaque(scanner: &CallScanner, call: &Call) -> bool {
    scanner.has_argument_unpack(call.opening_parenthesis)
        || writable_mode(scanner, call.opening_parenthesis, 1) && path_argument_is_opaque(scanner, call.opening_parenthesis, ArgumentSpec::new(0, &["file"]))
}

fn zipapp_target_is_opaque(scanner: &CallScanner, call: &Call) -> bool {
    if scanner.has_argument_unpack(call.opening_parenthesis) {
        return true;
    }
    match scanner.call_argument(call.opening_parenthesis, ArgumentSpec::new(1, &["target"])) {
        Some(target) if target.trim().trim_matches(['(', ')']).trim() != "None" => write_path_expression_is_opaque(scanner, &target),
        Some(_) | None => derived_zipapp_target_is_opaque(scanner, call),
    }
}

fn derived_zipapp_target_is_opaque(scanner: &CallScanner, call: &Call) -> bool {
    let Some(source) = literal_path_argument(scanner, call.opening_parenthesis, ArgumentSpec::new(0, &["source"])) else {
        return true;
    };
    let target = Path::new(&source).with_extension("pyz").to_string_lossy().replace('\\', "/");
    scanner.write_policy.is_opaque(&target)
}
