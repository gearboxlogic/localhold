use std::collections::BTreeSet;

use super::{collect_direct_rust_sources, untrusted_directory_change_with_quality_dispatcher, untrusted_directory_change_with_rust_tool, weakening_token_for_surface};

#[test]
fn protected_external_audit_roots_require_complete_validation() {
    let reviewed = r#"implementation_root=/trusted
repository_root=${LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT:-$implementation_root}
if [[ $repository_root != /* && ! $repository_root =~ ^[[:alpha:]]:[/\\] ]] || [[ ! -d $repository_root || -L $repository_root ]]; then
    exit 1
fi
repository_root=$(cd -- "$repository_root" && pwd -P)
LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT=$repository_root
readonly implementation_root repository_root LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT
export LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT
cd -- "$repository_root"
cargo test --locked
"#;
    assert!(!untrusted_directory_change_with_rust_tool(reviewed, false));
    assert!(untrusted_directory_change_with_rust_tool(
        &reviewed.replace("[[ ! -d $repository_root || -L $repository_root ]]", "[[ ! -d $repository_root ]]"),
        false
    ));
}

#[test]
fn quality_dispatchers_cannot_run_after_untrusted_directory_changes() {
    for source in [
        "cd quality/decoy; just check-quality",
        "pushd quality/decoy; make check-quality",
        "env --chdir=quality/decoy just maintainability",
    ] {
        assert!(untrusted_directory_change_with_quality_dispatcher(source, false), "{source}");
    }
    assert!(!untrusted_directory_change_with_quality_dispatcher("cd quality/decoy; just --version", false));
}

#[test]
fn direct_source_discovery_bounds_nested_ansi_c_text() {
    let mut sources = BTreeSet::new();
    assert!(!collect_direct_rust_sources(r#"write_manifest $'[package]\nname = "checker"'"#, true, &mut sources));
    assert!(sources.is_empty());
    assert!(collect_direct_rust_sources(r"$'rustc source.rs'", true, &mut sources));
    assert!(!collect_direct_rust_sources(
        "tool=${TOOL:?bootstrap must provide an absolute Cargo command}",
        true,
        &mut sources
    ));
    assert!(collect_direct_rust_sources("tool=${TOOL:?$(rustc source.rs)}", true, &mut sources));
}

#[test]
fn standalone_shells_cannot_assume_parent_errexit() {
    let masked = "#!/usr/bin/bash\ncargo clippy --locked -- -D warnings\ntrue\n";
    assert!(weakening_token_for_surface("quality/lint.data", masked));
    assert!(!weakening_token_for_surface("quality/lint.data", &masked.replace("#!/usr/bin/bash", "#!/usr/bin/bash -e")));
    assert!(!weakening_token_for_surface(
        "quality/lint.data",
        &masked.replace("#!/usr/bin/bash", "#!/usr/bin/bash\nset -e")
    ));
    assert!(!weakening_token_for_surface(
        "quality/lint.data",
        &masked.replace("#!/usr/bin/bash", "#!/usr/bin/env -S bash -euo pipefail")
    ));
    assert!(weakening_token_for_surface(
        "quality/lint.data",
        &masked.replace("#!/usr/bin/bash", "#!/usr/bin/pwsh -NoProfile")
    ));
}

#[test]
fn python_identifiers_do_not_enter_shell_assignment_flow() {
    let data_only = "cargo_manifest_path = Path('Cargo.toml')\nprint(cargo_manifest_path)\n";
    assert!(!weakening_token_for_surface("script/check.py", data_only));
    assert!(weakening_token_for_surface(
        "script/check.py",
        "import subprocess\ncargo = input()\nsubprocess.run([cargo, 'clippy'])\n"
    ));
}
