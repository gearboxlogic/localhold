use std::path::{Component, Path};

use super::{ArgumentSpec, Call, CallScanner, called_name, is_execution_surface_path, literal_value};

pub(super) fn named_temporary_file_is_opaque(scanner: &CallScanner, call: &Call) -> bool {
    if !matches!(called_name(&call.name), "NamedTemporaryFile" | "tempfile.NamedTemporaryFile") {
        return false;
    }
    let directory = match literal_component(scanner, call, ArgumentSpec::new(6, &["dir"])) {
        LiteralComponent::Missing => return false,
        LiteralComponent::Value(directory) => directory,
        LiteralComponent::Opaque => return true,
    };
    let prefix = match literal_component(scanner, call, ArgumentSpec::new(5, &["prefix"])) {
        LiteralComponent::Missing => String::new(),
        LiteralComponent::Value(prefix) => prefix,
        LiteralComponent::Opaque => return true,
    };
    let suffix = match literal_component(scanner, call, ArgumentSpec::new(4, &["suffix"])) {
        LiteralComponent::Missing => String::new(),
        LiteralComponent::Value(suffix) => suffix,
        LiteralComponent::Opaque => return true,
    };
    let directory = directory.replace('\\', "/");
    let path = Path::new(&directory);
    if path.is_absolute() || path.components().any(|component| !matches!(component, Component::CurDir | Component::Normal(_))) {
        return true;
    }
    let temporary_name = format!("{prefix}maintainability-check{suffix}");
    let candidate = path.join(temporary_name).to_string_lossy().replace('\\', "/");
    is_execution_surface_path(&candidate)
}

fn literal_component(scanner: &CallScanner, call: &Call, argument: ArgumentSpec) -> LiteralComponent {
    let Some(value) = scanner.call_argument(call.opening_parenthesis, argument) else {
        return LiteralComponent::Missing;
    };
    let value = value.trim();
    if value == "None" {
        return LiteralComponent::Missing;
    }
    literal_value(value).map_or(LiteralComponent::Opaque, LiteralComponent::Value)
}

enum LiteralComponent {
    Missing,
    Value(String),
    Opaque,
}
