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
        if value.contains(':') || path.is_absolute() || path.components().any(|component| !matches!(component, Component::CurDir | Component::Normal(_))) {
            return true;
        }
        let Some(normalized) = windows_equivalent(&value) else {
            return true;
        };
        if is_execution_surface(&normalized) || self.execution_surfaces.contains(&normalized) {
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
            match resolve_component(&mut resolved, component, &root) {
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
        is_execution_surface(&relative) || self.execution_surfaces.contains(&relative)
    }
}

enum ResolvedComponent {
    Existing,
    Missing,
    Opaque,
}

fn resolve_component(resolved: &mut PathBuf, component: &OsStr, root: &Path) -> ResolvedComponent {
    resolved.push(component);
    match fs::symlink_metadata(&resolved) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let Ok(target) = fs::canonicalize(&resolved) else {
                return ResolvedComponent::Opaque;
            };
            if !target.starts_with(root) {
                return ResolvedComponent::Opaque;
            }
            *resolved = target;
            ResolvedComponent::Existing
        }
        Ok(_) => ResolvedComponent::Existing,
        Err(error) if error.kind() == ErrorKind::NotFound => ResolvedComponent::Missing,
        Err(_) => ResolvedComponent::Opaque,
    }
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
