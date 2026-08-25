pub(super) fn is_weakening_environment_name(name: &str) -> bool {
    if name == "ENV" {
        return true;
    }
    let name = name.to_ascii_uppercase();
    if name.starts_with("BASH_FUNC_") || is_runtime_code_loading_environment_name(&name) {
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
            | "COMPILER_PATH"
            | "GCC_EXEC_PREFIX"
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
            | "CARGO_UNSTABLE_SCRIPT"
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
    ) || is_native_build_environment_name(&name)
        || name.starts_with("CARGO_ALIAS_")
        || name.starts_with("CARGO_TARGET_") && (name.ends_with("_RUSTFLAGS") || name.ends_with("_RUSTDOCFLAGS") || name.ends_with("_LINKER") || name.ends_with("_RUNNER"))
        || name.starts_with("GIT_")
}

fn is_native_build_environment_name(name: &str) -> bool {
    const SELECTORS: &[&str] = &["AR", "ARFLAGS", "CC", "CFLAGS", "CXX", "CXXFLAGS", "LDFLAGS", "NVCC", "RANLIB", "RANLIBFLAGS"];
    matches!(name, "CPATH" | "C_INCLUDE_PATH" | "CPLUS_INCLUDE_PATH" | "OBJC_INCLUDE_PATH")
        || is_cmake_toolchain_environment_name(name)
        || name == "CROSS_COMPILE"
        || SELECTORS.iter().any(|selector| {
            name == *selector
                || name
                    .strip_prefix(selector)
                    .and_then(|suffix| suffix.strip_prefix('_'))
                    .is_some_and(|suffix| suffix.contains(['-', '_', '.']))
                || name.strip_prefix("HOST_") == Some(*selector)
                || name.strip_prefix("TARGET_") == Some(*selector)
        })
}

fn is_cmake_toolchain_environment_name(name: &str) -> bool {
    name == "CMAKE_TOOLCHAIN_FILE" || name.starts_with("CMAKE_TOOLCHAIN_FILE_") || name.ends_with("_CMAKE_TOOLCHAIN_FILE") || name.contains("_CMAKE_TOOLCHAIN_FILE_")
}

fn is_runtime_code_loading_environment_name(name: &str) -> bool {
    matches!(
        name,
        "BASHOPTS"
            | "BASH_ENV"
            | "BROWSER"
            | "CARGO_DOC_BROWSER"
            | "CCC_OVERRIDE_OPTIONS"
            | "GCONV_PATH"
            | "JAVA_TOOL_OPTIONS"
            | "JDK_JAVA_OPTIONS"
            | "JDK_JAVAC_OPTIONS"
            | "SHELLOPTS"
            | "LD_AUDIT"
            | "LD_LIBRARY_PATH"
            | "LD_PRELOAD"
            | "NODE_OPTIONS"
            | "OPENSSL_CONF"
            | "OPENSSL_CONF_INCLUDE"
            | "OPENSSL_ENGINES"
            | "OPENSSL_MODULES"
            | "PS4"
            | "PERL5LIB"
            | "PERL5OPT"
            | "PERLLIB"
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
            | "RIPGREP_CONFIG_PATH"
            | "RUBYOPT"
            | "ZIPOPT"
            | "_CL_"
            | "_JAVA_OPTIONS"
    )
}

pub(super) fn is_weakening_environment_assignment_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("CARGO")
        || name.eq_ignore_ascii_case("CL")
        || name.eq_ignore_ascii_case("EDITOR")
        || name.eq_ignore_ascii_case("VISUAL")
        || name.eq_ignore_ascii_case("ZIP")
        || is_weakening_environment_name(name)
}

pub(super) fn is_case_insensitive_weakening_environment_assignment_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("ENV") || is_weakening_environment_assignment_name(name)
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
    fn browser_selection_environment_is_weakening() {
        for name in ["BROWSER", "CARGO_DOC_BROWSER"] {
            assert!(is_weakening_environment_name(name), "{name}");
            assert!(is_weakening_environment_name(&name.to_ascii_lowercase()), "{name}");
        }
    }

    #[test]
    fn jvm_code_loading_environment_is_weakening() {
        for name in ["JAVA_TOOL_OPTIONS", "JDK_JAVA_OPTIONS", "JDK_JAVAC_OPTIONS", "_JAVA_OPTIONS"] {
            assert!(is_weakening_environment_name(name), "{name}");
            assert!(is_weakening_environment_name(&name.to_ascii_lowercase()), "{name}");
        }
    }

    #[test]
    fn compiler_executable_search_paths_are_weakening() {
        for name in ["CCC_OVERRIDE_OPTIONS", "COMPILER_PATH", "GCC_EXEC_PREFIX", "_CL_"]
            .into_iter()
            .chain(["CPATH", "C_INCLUDE_PATH", "CPLUS_INCLUDE_PATH", "OBJC_INCLUDE_PATH"])
        {
            assert!(is_weakening_environment_name(name), "{name}");
            assert!(is_weakening_environment_name(&name.to_ascii_lowercase()), "{name}");
        }
        assert!(is_weakening_environment_assignment_name("CL"));
        assert!(is_weakening_environment_assignment_name("cl"));
        assert!(!is_weakening_environment_name("CL"));
    }

    #[test]
    fn native_build_tool_environment_is_weakening() {
        for name in [
            "CC",
            "CC_x86_64-unknown-linux-gnu",
            "CC_x86_64_unknown_linux_gnu",
            "HOST_CC",
            "TARGET_CC",
            "CXX_aarch64-pc-windows-msvc",
            "AR",
            "TARGET_RANLIB",
            "NVCC_x86_64_unknown_linux_gnu",
            "CFLAGS",
            "HOST_CXXFLAGS",
            "LDFLAGS",
            "ARFLAGS_x86_64_unknown_linux_gnu",
            "TARGET_RANLIBFLAGS",
            "CROSS_COMPILE",
            "CMAKE_TOOLCHAIN_FILE",
            "CMAKE_TOOLCHAIN_FILE_x86_64-unknown-linux-gnu",
            "CMAKE_TOOLCHAIN_FILE_x86_64_unknown_linux_gnu",
            "HOST_CMAKE_TOOLCHAIN_FILE",
            "TARGET_CMAKE_TOOLCHAIN_FILE",
            "AWS_LC_SYS_CMAKE_TOOLCHAIN_FILE",
            "AWS_LC_SYS_CMAKE_TOOLCHAIN_FILE_x86_64_unknown_linux_gnu",
        ] {
            assert!(is_weakening_environment_name(name), "{name}");
            assert!(is_weakening_environment_name(&name.to_ascii_lowercase()), "{name}");
        }
        for name in [
            "CCACHE",
            "CCACHE_DIR",
            "ACCOUNT",
            "SUCCESS",
            "CXX_STANDARD",
            "HOST_CCACHE",
            "CMAKE_TOOLCHAINS_FILE",
            "CMAKE_TOOLCHAIN_FILENAME",
        ] {
            assert!(!is_weakening_environment_name(name), "{name}");
        }
    }

    #[test]
    fn archive_command_environment_is_weakening() {
        assert!(is_weakening_environment_name("ZIPOPT"));
        assert!(is_weakening_environment_name("zipopt"));
        assert!(is_weakening_environment_assignment_name("ZIP"));
        assert!(is_weakening_environment_assignment_name("zip"));
        assert!(!is_weakening_environment_name("ZIP"));
    }

    #[test]
    fn editor_selection_environment_is_weakening() {
        for name in ["EDITOR", "VISUAL"] {
            assert!(is_weakening_environment_assignment_name(name), "{name}");
            assert!(is_weakening_environment_assignment_name(&name.to_ascii_lowercase()), "{name}");
            assert!(!is_weakening_environment_name(name), "{name}");
        }
    }

    #[test]
    fn ruby_code_loading_environment_is_weakening() {
        assert!(is_weakening_environment_name("RUBYOPT"));
        assert!(is_weakening_environment_name("rubyopt"));
    }

    #[test]
    fn perl_code_loading_environment_is_weakening() {
        for name in ["PERL5LIB", "PERL5OPT", "PERLLIB"] {
            assert!(is_weakening_environment_name(name));
            assert!(is_weakening_environment_name(&name.to_ascii_lowercase()));
        }
    }

    #[test]
    fn inherited_bash_options_are_weakening() {
        assert!(is_weakening_environment_name("BASHOPTS"));
        assert!(is_weakening_environment_name("bashopts"));
    }

    #[test]
    fn glibc_conversion_module_path_is_weakening() {
        assert!(is_weakening_environment_name("GCONV_PATH"));
        assert!(is_weakening_environment_name("gconv_path"));
    }

    #[test]
    fn openssl_configuration_and_module_paths_are_weakening() {
        for name in ["OPENSSL_CONF", "OPENSSL_CONF_INCLUDE", "OPENSSL_ENGINES", "OPENSSL_MODULES"] {
            assert!(is_weakening_environment_name(name), "{name}");
            assert!(is_weakening_environment_name(&name.to_ascii_lowercase()), "{name}");
        }
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
    fn cargo_script_mode_environment_is_weakening() {
        assert!(is_weakening_environment_name("CARGO_UNSTABLE_SCRIPT"));
        assert!(is_weakening_environment_name("cargo_unstable_script"));
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
        assert!(is_weakening_environment_name("ENV"));
        assert!(!is_weakening_environment_name("env"));
        assert!(is_weakening_environment_name("SHELLOPTS"));
        assert!(is_weakening_environment_name("shellopts"));
    }

    #[test]
    fn xtrace_prompt_is_weakening() {
        assert!(is_weakening_environment_name("PS4"));
        assert!(is_weakening_environment_name("ps4"));
    }

    #[test]
    fn exported_shell_functions_are_weakening() {
        assert!(is_weakening_environment_name("BASH_FUNC_just%%"));
        assert!(is_weakening_environment_name("bash_func_just%%"));
    }

    #[test]
    fn ripgrep_configuration_is_weakening() {
        assert!(is_weakening_environment_name("RIPGREP_CONFIG_PATH"));
        assert!(is_weakening_environment_name("ripgrep_config_path"));
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
