use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::model::{ROOT_LOCKFILE, ROOT_MANIFEST};

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceProfile {
    #[serde(rename = "current_sha256")]
    pub(super) current: String,
    #[serde(rename = "preapproved_next_sha256")]
    pub(super) pending: Option<String>,
    #[serde(rename = "retired_sha256")]
    pub(super) retired: Vec<String>,
}

impl SourceProfile {
    pub(super) fn validate(&self) -> Result<()> {
        validate_digest("current", &self.current)?;
        if let Some(next) = &self.pending {
            validate_digest("preapproved next", next)?;
            if next == &self.current {
                bail!("maintainability analyzer source profile pending digest must differ from current");
            }
        }
        let mut retired = BTreeSet::new();
        for digest in &self.retired {
            validate_digest("retired", digest)?;
            if !retired.insert(digest) {
                bail!("maintainability analyzer source profile retired digests must be unique");
            }
        }
        if retired.contains(&self.current) || self.pending.as_ref().is_some_and(|next| retired.contains(next)) {
            bail!("maintainability analyzer source profile active digests cannot be retired");
        }
        Ok(())
    }

    pub(super) fn require_current(&self, observed: &str) -> Result<()> {
        if observed != self.current {
            bail!("maintainability analyzer source profile mismatch: expected={}, observed={observed}", self.current);
        }
        Ok(())
    }

    pub(super) fn compare_previous(&self, previous: &Self) -> Result<()> {
        if self.current == previous.current && self.retired == previous.retired {
            return compare_pending(self.pending.as_deref(), previous.pending.as_deref(), &self.current);
        }
        let Some(next) = previous.pending.as_deref() else {
            bail!("maintainability analyzer source profile current digest can change only to a previously preapproved next digest");
        };
        let mut expected_retired = previous.retired.clone();
        expected_retired.push(previous.current.clone());
        if self.current != next || self.pending.is_some() || self.retired != expected_retired {
            bail!("maintainability analyzer source profile promotion must atomically promote pending, retire current, and clear pending");
        }
        Ok(())
    }
}

pub(super) fn fingerprint(workspace: &Path, sources: &BTreeSet<String>) -> Result<String> {
    let mut inputs = BTreeSet::from([ROOT_MANIFEST.to_owned(), ROOT_LOCKFILE.to_owned()]);
    inputs.extend(sources.iter().cloned());
    let mut digest = Sha256::new();
    for path in inputs {
        let bytes = fs::read(workspace.join(&path)).with_context(|| format!("read maintainability analyzer source-profile input {path:?}"))?;
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path.as_bytes());
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn compare_pending(current: Option<&str>, previous: Option<&str>, active: &str) -> Result<()> {
    match (previous, current) {
        (None | Some(_), None) => Ok(()),
        (None, Some(next)) if next != active => Ok(()),
        (Some(old), Some(next)) if old == next => Ok(()),
        (None | Some(_), Some(_)) => bail!("maintainability analyzer source profile may stage only one pending digest and cannot replace it"),
    }
}

fn validate_digest(label: &str, value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        bail!("maintainability analyzer source profile {label} digest must be lowercase SHA-256");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn profile(current: &str, next: Option<&str>, retired: &[&str]) -> SourceProfile {
        SourceProfile {
            current: current.to_owned(),
            pending: next.map(str::to_owned),
            retired: retired.iter().map(|digest| (*digest).to_owned()).collect(),
        }
    }

    #[test]
    fn profile_transitions_require_stage_then_atomic_promotion() {
        let initial = profile(A, None, &[]);
        let staged = profile(A, Some(B), &[]);
        staged.compare_previous(&initial).expect("stage pending");
        staged.require_current(A).expect("pending does not replace current");
        assert!(staged.require_current(B).is_err());
        staged.compare_previous(&staged).expect("unchanged pending");
        initial.compare_previous(&staged).expect("cancel pending");
        profile(B, None, &[A]).compare_previous(&staged).expect("promote pending");
    }

    #[test]
    fn replacement_unstaged_stale_and_resurrected_profiles_are_rejected() {
        let initial = profile(A, None, &[]);
        let staged = profile(A, Some(B), &[]);
        assert!(profile(A, Some(C), &[]).compare_previous(&staged).is_err());
        assert!(profile(B, None, &[A]).compare_previous(&initial).is_err());
        assert!(profile(B, Some(C), &[A]).compare_previous(&staged).is_err());
        assert!(profile(A, None, &[]).compare_previous(&profile(B, None, &[A])).is_err());
    }

    #[test]
    fn fingerprint_covers_sources_manifest_and_lockfile() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let source = "tools/maintainability/src/main.rs";
        for (path, bytes) in [(source, "fn main() {}\n"), (ROOT_MANIFEST, "[package]\n"), (ROOT_LOCKFILE, "lock\n")] {
            let target = workspace.path().join(path);
            fs::create_dir_all(target.parent().expect("input parent")).expect("input directory");
            fs::write(target, bytes).expect("profile input");
        }
        let sources = BTreeSet::from([source.to_owned()]);
        let original = fingerprint(workspace.path(), &sources).expect("original profile");
        for path in [source, ROOT_MANIFEST, ROOT_LOCKFILE] {
            fs::write(workspace.path().join(path), "changed\n").expect("mutate profile input");
            assert_ne!(fingerprint(workspace.path(), &sources).expect("changed profile"), original, "{path}");
            fs::write(
                workspace.path().join(path),
                match path {
                    ROOT_MANIFEST => "[package]\n",
                    ROOT_LOCKFILE => "lock\n",
                    _ => "fn main() {}\n",
                },
            )
            .expect("restore profile input");
        }
    }

    #[test]
    fn excluded_external_rust_mutations_are_rejected_as_compiler_inputs() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let source = "tools/maintainability/src/main.rs";
        let external = "tools/maintainability/quality/helper.rs";
        let manifest = concat!(
            "[package]\nname = 'maintainability'\nbuild = false\n",
            "autolib = false\nautobins = false\nautoexamples = false\nautotests = false\nautobenches = false\n",
            "[workspace]\n",
            "[[bin]]\nname = 'helper'\npath = 'quality/helper.rs'\n",
        );
        for (path, bytes) in [
            (source, "fn main() {}\n"),
            (external, "fn helper() {}\n"),
            (ROOT_MANIFEST, manifest),
            (ROOT_LOCKFILE, "lock\n"),
        ] {
            let target = workspace.path().join(path);
            fs::create_dir_all(target.parent().expect("input parent")).expect("input directory");
            fs::write(target, bytes).expect("profile input");
        }
        let profiled = BTreeSet::from([source.to_owned()]);
        let original = fingerprint(workspace.path(), &profiled).expect("original profile");
        fs::write(workspace.path().join(external), "fn changed() {}\n").expect("mutate excluded compiler input");
        assert_eq!(fingerprint(workspace.path(), &profiled).expect("unchanged profile"), original);

        let sources = std::collections::BTreeMap::from([(source.to_owned(), "fn main() {}\n".to_owned())]);
        let error = super::super::classification::validate_compiler_inputs(&sources, manifest).unwrap_err();
        assert!(error.to_string().contains("profiled Rust source"), "{error:#}");
    }

    #[test]
    fn excluded_path_dependency_mutations_cannot_enter_a_reviewed_profile() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let source = "tools/maintainability/src/main.rs";
        let external = "tools/helper/src/lib.rs";
        let manifest = concat!(
            "[package]\nname = 'maintainability'\nbuild = false\n",
            "autolib = false\nautobins = false\nautoexamples = false\nautotests = false\nautobenches = false\n",
            "[workspace]\n[dependencies.helper]\npath = '../../helper'\n",
        );
        for (path, bytes) in [
            (source, "fn main() {}\n"),
            (external, "pub fn helper() {}\n"),
            (ROOT_MANIFEST, manifest),
            (ROOT_LOCKFILE, "lock\n"),
        ] {
            let target = workspace.path().join(path);
            fs::create_dir_all(target.parent().expect("input parent")).expect("input directory");
            fs::write(target, bytes).expect("profile input");
        }
        let profiled = BTreeSet::from([source.to_owned()]);
        let original = fingerprint(workspace.path(), &profiled).expect("original profile");
        fs::write(workspace.path().join(external), "pub fn changed() {}\n").expect("mutate excluded path dependency");
        assert_eq!(fingerprint(workspace.path(), &profiled).expect("unchanged profile"), original);

        let sources = std::collections::BTreeMap::from([(source.to_owned(), "fn main() {}\n".to_owned())]);
        let error = super::super::classification::validate_compiler_inputs(&sources, manifest).unwrap_err();
        assert!(error.to_string().contains("external or inherited source"), "{error:#}");
    }
}
