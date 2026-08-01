pub(super) fn is_weakening_environment_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    if is_runtime_code_loading_environment_name(&name) {
        return true;
    }
    matches!(
        name.as_str(),
        "RUSTFLAGS"
            | "CARGO_ENCODED_RUSTFLAGS"
            | "RUSTDOCFLAGS"
            | "CARGO_ENCODED_RUSTDOCFLAGS"
            | "CLIPPY_ARGS"
            | "CLIPPY_CONF_DIR"
            | "RUSTC"
            | "RUSTDOC"
            | "RUSTC_BOOTSTRAP"
            | "RUSTC_WRAPPER"
            | "RUSTC_WORKSPACE_WRAPPER"
            | "RUSTUP_DIST_SERVER"
            | "RUSTUP_HOME"
            | "RUSTUP_TOOLCHAIN"
            | "RUSTUP_UPDATE_ROOT"
            | "TAR_OPTIONS"
            | "CARGO_HOME"
            | "CARGO_BUILD_TARGET"
            | "CARGO_TARGET_DIR"
            | "CARGO_BUILD_RUSTFLAGS"
            | "CARGO_BUILD_RUSTDOCFLAGS"
            | "CARGO_BUILD_RUSTC"
            | "CARGO_BUILD_RUSTDOC"
            | "CARGO_BUILD_RUSTC_WRAPPER"
            | "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"
            | "GITHUB_ACTIONS"
            | "GITHUB_ENV"
            | "GITHUB_EVENT_PATH"
            | "GITHUB_PATH"
            | "GITHUB_SHA"
            | "LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT"
            | "LOCALHOLD_MAINTAINABILITY_BASE_REV"
            | "LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_ACTUAL_SHA256"
            | "LOCALHOLD_MAINTAINABILITY_BOOTSTRAP_SHA256"
            | "LOCALHOLD_MAINTAINABILITY_CARGO"
            | "LOCALHOLD_MAINTAINABILITY_CARGO_CLIPPY"
            | "LOCALHOLD_MAINTAINABILITY_CARGO_FMT"
            | "LOCALHOLD_MAINTAINABILITY_GIT"
            | "LOCALHOLD_MAINTAINABILITY_RUSTC"
            | "LOCALHOLD_MAINTAINABILITY_RUSTUP"
            | "GNUMAKEFLAGS"
            | "MAKEFILES"
            | "MAKEFLAGS"
            | "MFLAGS"
            | "MISE_CONFIG_DIR"
            | "MISE_CONFIG_FILE"
            | "MISE_CEILING_PATHS"
            | "MISE_DEFAULT_CONFIG_FILENAME"
            | "MISE_ENV_FILE"
            | "MISE_GLOBAL_CONFIG_FILE"
            | "MISE_GLOBAL_CONFIG_ROOT"
            | "MISE_OVERRIDE_CONFIG_FILENAMES"
            | "MISE_SYSTEM_CONFIG_DIR"
            | "MISE_SYSTEM_CONFIG_FILE"
            | "MISE_SYSTEM_DIR"
    ) || name.starts_with("CARGO_ALIAS_")
        || name.starts_with("CARGO_TARGET_") && (name.ends_with("_RUSTFLAGS") || name.ends_with("_RUSTDOCFLAGS") || name.ends_with("_LINKER") || name.ends_with("_RUNNER"))
        || name.starts_with("GIT_")
}

fn is_runtime_code_loading_environment_name(name: &str) -> bool {
    matches!(
        name,
        "BASH_ENV"
            | "SHELLOPTS"
            | "LD_AUDIT"
            | "LD_LIBRARY_PATH"
            | "LD_PRELOAD"
            | "NODE_OPTIONS"
            | "PYTHONBREAKPOINT"
            | "PYTHONCASEOK"
            | "PYTHONEXECUTABLE"
            | "PYTHONHOME"
            | "PYTHONINSPECT"
            | "PYTHONPATH"
            | "PYTHONPLATLIBDIR"
            | "PYTHONSTARTUP"
            | "PYTHONUSERBASE"
            | "PYTHONWARNINGS"
            | "RUBYOPT"
    )
}

pub(super) fn is_weakening_environment_assignment_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("CARGO") || is_weakening_environment_name(name)
}

#[cfg(test)]
mod tests {
    use super::{is_weakening_environment_assignment_name, is_weakening_environment_name};

    #[test]
    fn python_code_loading_environment_is_weakening() {
        for name in [
            "PYTHONBREAKPOINT",
            "PYTHONCASEOK",
            "PYTHONEXECUTABLE",
            "PYTHONHOME",
            "PYTHONINSPECT",
            "PYTHONPATH",
            "PYTHONPLATLIBDIR",
            "PYTHONSTARTUP",
            "PYTHONUSERBASE",
            "PYTHONWARNINGS",
        ] {
            assert!(is_weakening_environment_name(name), "{name}");
            assert!(is_weakening_environment_name(&name.to_ascii_lowercase()), "{name}");
        }
        assert!(!is_weakening_environment_name("PYTHONSAFEPATH"));
        assert!(!is_weakening_environment_name("PYTHONUNBUFFERED"));
    }

    #[test]
    fn node_code_loading_environment_is_weakening() {
        assert!(is_weakening_environment_name("NODE_OPTIONS"));
        assert!(is_weakening_environment_name("node_options"));
    }

    #[test]
    fn ruby_code_loading_environment_is_weakening() {
        assert!(is_weakening_environment_name("RUBYOPT"));
        assert!(is_weakening_environment_name("rubyopt"));
    }

    #[test]
    fn cargo_is_weakening_only_in_environment_assignment_positions() {
        assert!(is_weakening_environment_assignment_name("CARGO"));
        assert!(is_weakening_environment_assignment_name("cargo"));
        assert!(!is_weakening_environment_name("Cargo"));
    }

    #[test]
    fn cargo_target_directory_is_a_weakening_environment_channel() {
        assert!(is_weakening_environment_name("CARGO_TARGET_DIR"));
        assert!(is_weakening_environment_name("cargo_target_dir"));
    }

    #[test]
    fn github_cross_step_mutation_files_are_weakening() {
        for name in ["GITHUB_ENV", "GITHUB_PATH"] {
            assert!(is_weakening_environment_name(name), "{name}");
            assert!(is_weakening_environment_name(&name.to_ascii_lowercase()), "{name}");
        }
    }

    #[test]
    fn inherited_shell_options_are_weakening() {
        assert!(is_weakening_environment_name("SHELLOPTS"));
        assert!(is_weakening_environment_name("shellopts"));
    }

    #[test]
    fn rustup_toolchain_selectors_are_weakening() {
        for name in ["RUSTUP_HOME", "RUSTUP_TOOLCHAIN"] {
            assert!(is_weakening_environment_name(name), "{name}");
            assert!(is_weakening_environment_name(&name.to_ascii_lowercase()), "{name}");
        }
    }

    #[test]
    fn maintainability_audit_root_is_weakening() {
        assert!(is_weakening_environment_name("LOCALHOLD_MAINTAINABILITY_AUDIT_ROOT"));
    }
}
