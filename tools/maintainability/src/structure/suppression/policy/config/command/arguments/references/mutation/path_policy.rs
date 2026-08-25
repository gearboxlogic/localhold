use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use super::{ValueSemantics, path};

pub(in crate::structure::suppression::policy::config::command::arguments::references) struct Policy {
    workspace: PathBuf,
    protected_inputs: BTreeSet<String>,
}

impl Policy {
    pub(in crate::structure::suppression::policy::config::command::arguments::references) fn for_workspace(
        workspace: &Path,
        execution_surfaces: &BTreeSet<String>,
        tracked_paths: &BTreeSet<String>,
    ) -> Self {
        let mut protected_inputs = execution_surfaces.iter().filter_map(|path| normalized_key(path)).collect::<BTreeSet<_>>();
        protected_inputs.extend(
            tracked_paths
                .iter()
                .filter_map(|path| normalized_key(path).map(|normalized| (path, normalized)))
                .filter(|(path, _)| super::super::super::super::is_protected_check_input(path))
                .map(|(_, normalized)| normalized),
        );
        Self {
            workspace: workspace.to_path_buf(),
            protected_inputs,
        }
    }

    fn redirected_target_is_opaque(&self, path: &Path) -> bool {
        let Ok(root) = fs::canonicalize(&self.workspace) else {
            return true;
        };
        let mut resolved = root.clone();
        let mut components = path.components().filter_map(|component| match component {
            Component::CurDir => None,
            Component::Normal(component) => Some(component),
            _ => unreachable!("relative target components were validated"),
        });
        while let Some(component) = components.next() {
            resolved.push(component);
            match fs::symlink_metadata(&resolved) {
                Ok(metadata) if metadata.file_type().is_symlink() => return true,
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    resolved.extend(components);
                    break;
                }
                Err(_) => return true,
            }
        }
        let Ok(relative) = resolved.strip_prefix(root) else {
            return true;
        };
        relative.to_str().and_then(normalized_key).is_none_or(|target| self.is_protected_or_ancestor(&target))
    }

    fn is_protected_or_ancestor(&self, target: &str) -> bool {
        self.protected_inputs
            .iter()
            .any(|protected| protected == target || protected.strip_prefix(target).is_some_and(|suffix| suffix.starts_with('/')))
    }
}

pub(super) fn target_is_opaque(policy: Option<&Policy>, candidate: &str, semantics: ValueSemantics) -> bool {
    if candidate == "/dev/null" {
        return false;
    }
    let Some(normalized) = path::normalize_literal_with_semantics(candidate, semantics) else {
        return true;
    };
    if has_dos_short_name(Path::new(&normalized)) {
        return true;
    }
    let Some(key) = normalized_key(&normalized) else {
        return true;
    };
    if super::super::super::super::is_protected_check_input(&normalized)
        || super::super::super::super::is_protected_check_input(&key)
        || policy.is_some_and(|policy| policy.is_protected_or_ancestor(&key) || policy.redirected_target_is_opaque(Path::new(&normalized)))
    {
        return true;
    }
    false
}

fn normalized_key(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let mut normalized = Vec::new();
    for component in Path::new(&path).components() {
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

fn has_dos_short_name(path: &Path) -> bool {
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
        && generation.bytes().all(|digit| digit.is_ascii_digit())
}

const fn valid_dos_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '$' | '%' | '\'' | '-' | '_' | '@' | '`' | '!' | '(' | ')' | '{' | '}' | '^' | '#' | '&')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_and_absolute_targets_fail_closed() {
        for target in [
            "src/../Justfile",
            r"src\..\Justfile",
            "/proc/self/cwd/Justfile",
            r"C:\repo\Justfile",
            "CARGO~1.TOM",
            "Cargo.toml.",
            "Cargo.toml ",
        ] {
            assert!(target_is_opaque(None, target, ValueSemantics::Literal), "{target}");
        }
        assert!(!target_is_opaque(None, "target/report$*.txt", ValueSemantics::Literal));
        assert!(target_is_opaque(None, "target/report$*.txt", ValueSemantics::Shell));
        assert!(!target_is_opaque(None, "/dev/null", ValueSemantics::Shell));
        assert!(!target_is_opaque(None, "NUL", ValueSemantics::Literal));
        for target in ["target/report~notes.txt", "target/verylong~1.txt", "target/report~1.long"] {
            assert!(!target_is_opaque(None, target, ValueSemantics::Literal), "{target}");
        }
    }

    #[test]
    fn nul_is_only_a_sink_when_it_is_not_a_protected_repository_name() {
        let workspace = tempfile::tempdir().expect("workspace");
        fs::write(workspace.path().join("NUL"), "#!/bin/sh\n").expect("protected target");
        let surfaces = BTreeSet::from(["NUL".to_owned()]);
        let tracked = surfaces.clone();
        let policy = Policy::for_workspace(workspace.path(), &surfaces, &tracked);

        assert!(target_is_opaque(Some(&policy), "NUL", ValueSemantics::Shell));
        assert!(target_is_opaque(Some(&policy), "nul", ValueSemantics::Literal));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_target_components_fail_closed() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        fs::create_dir_all(workspace.path().join("docs")).expect("docs");
        fs::create_dir_all(workspace.path().join("script")).expect("script");
        fs::write(workspace.path().join("script/check.sh"), "#!/bin/sh\n").expect("protected target");
        symlink("../script/check.sh", workspace.path().join("docs/check")).expect("target alias");
        let surfaces = BTreeSet::from(["script/check.sh".to_owned()]);
        let tracked = BTreeSet::from(["docs/check".to_owned(), "script/check.sh".to_owned()]);
        let policy = Policy::for_workspace(workspace.path(), &surfaces, &tracked);

        assert!(target_is_opaque(Some(&policy), "docs/check", ValueSemantics::Shell));
        assert!(!target_is_opaque(Some(&policy), "target/report", ValueSemantics::Shell));
    }
}
