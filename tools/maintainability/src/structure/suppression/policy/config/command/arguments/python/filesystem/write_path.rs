use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Default)]
pub(super) struct Policy {
    workspace: Option<PathBuf>,
    execution_surfaces: BTreeSet<String>,
}

impl Policy {
    pub(super) fn for_workspace(workspace: &Path, execution_surfaces: &[String]) -> Self {
        Self {
            workspace: Some(workspace.to_path_buf()),
            execution_surfaces: execution_surfaces.iter().filter_map(|path| windows_equivalent(path)).collect(),
        }
    }

    pub(super) fn is_opaque(&self, value: &str) -> bool {
        if value.contains(['$', '%', '{', '}', '*', '?']) {
            return true;
        }
        let value = value.replace('\\', "/");
        let path = Path::new(&value);
        if value.contains(':')
            || path.is_absolute()
            || path.components().any(|component| !matches!(component, Component::CurDir | Component::Normal(_)))
            || has_dos_short_name_component(path)
        {
            return true;
        }
        let Some(normalized) = windows_equivalent(&value) else {
            return true;
        };
        if is_execution_surface(&normalized) || self.is_execution_surface_or_ancestor(&normalized) {
            return true;
        }
        self.workspace.as_deref().is_some_and(|workspace| self.redirected_path_is_opaque(workspace, path))
    }

    fn redirected_path_is_opaque(&self, workspace: &Path, path: &Path) -> bool {
        let Ok(root) = fs::canonicalize(workspace) else {
            return true;
        };
        let mut resolved = root.clone();
        let components = path
            .components()
            .filter_map(|component| match component {
                Component::CurDir => None,
                Component::Normal(component) => Some(component),
                _ => unreachable!("relative path components were validated"),
            })
            .peekable();
        let mut remaining = components;
        while let Some(component) = remaining.next() {
            match resolve_component(&mut resolved, component) {
                ResolvedComponent::Existing => {}
                ResolvedComponent::Missing => {
                    resolved.extend(remaining);
                    break;
                }
                ResolvedComponent::Opaque => return true,
            }
        }
        let Ok(relative) = resolved.strip_prefix(&root) else {
            return true;
        };
        let Some(relative) = relative.to_str().and_then(windows_equivalent) else {
            return true;
        };
        is_execution_surface(&relative) || self.is_execution_surface_or_ancestor(&relative)
    }

    fn is_execution_surface_or_ancestor(&self, path: &str) -> bool {
        self.execution_surfaces
            .iter()
            .any(|surface| surface == path || surface.strip_prefix(path).is_some_and(|descendant| descendant.starts_with('/')))
    }
}

enum ResolvedComponent {
    Existing,
    Missing,
    Opaque,
}

fn resolve_component(resolved: &mut PathBuf, component: &OsStr) -> ResolvedComponent {
    resolved.push(component);
    match fs::symlink_metadata(&resolved) {
        // A script can relocate a symlink after analysis, so its current canonical target is not a stable write boundary.
        Ok(metadata) if metadata.file_type().is_symlink() => ResolvedComponent::Opaque,
        Ok(_) => ResolvedComponent::Existing,
        Err(error) if error.kind() == ErrorKind::NotFound => ResolvedComponent::Missing,
        Err(_) => ResolvedComponent::Opaque,
    }
}

fn has_dos_short_name_component(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(component) => component.to_str().is_some_and(dos_short_name_form),
        _ => false,
    })
}

fn dos_short_name_form(component: &str) -> bool {
    let component = component.trim_end_matches([' ', '.']);
    let mut name = component.split('.');
    let stem = name.next().unwrap_or_default();
    let extension = name.next();
    if name.next().is_some() || extension.is_some_and(|extension| extension.is_empty() || extension.len() > 3 || !extension.chars().all(valid_dos_character)) {
        return false;
    }
    let Some((prefix, generation)) = stem.rsplit_once('~') else {
        return false;
    };
    !prefix.is_empty()
        && !prefix.contains('~')
        && prefix.chars().all(valid_dos_character)
        && stem.len() <= 8
        && generation.as_bytes().first().is_some_and(|digit| matches!(digit, b'1'..=b'9'))
        && generation.chars().all(|digit| digit.is_ascii_digit())
}

const fn valid_dos_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '$' | '%' | '\'' | '-' | '_' | '@' | '`' | '!' | '(' | ')' | '{' | '}' | '^' | '#' | '&')
}

fn windows_equivalent(value: &str) -> Option<String> {
    let value = value.replace('\\', "/");
    let path = Path::new(&value);
    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => {
                let component = component.to_str()?.trim_end_matches([' ', '.']);
                if component.is_empty() {
                    return None;
                }
                normalized.push(component.to_ascii_lowercase());
            }
            _ => return None,
        }
    }
    (!normalized.is_empty()).then(|| normalized.join("/"))
}

fn is_execution_surface(path: &str) -> bool {
    let basename = path.rsplit('/').next().unwrap_or_default();
    basename == "gnumakefile" || super::super::super::super::is_execution_surface(path)
}
