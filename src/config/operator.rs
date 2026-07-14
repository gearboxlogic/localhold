//! Side-effect-conscious configuration operator commands.

use std::{fs::OpenOptions, io::Write as _, path::Path};

use serde::Serialize;

use super::{Config, user_config_candidates, user_config_dir};
use crate::error::EngineError;

/// Machine-readable configuration report schema version.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Exit code used when configuration is valid.
pub const EXIT_VALID: i32 = 0;
/// Exit code used when configuration is invalid or cannot be loaded.
pub const EXIT_INVALID: i32 = 1;

/// Load the effective configuration while rejecting malformed environment overrides.
///
/// Unlike the normal server startup loader, this operator-facing loader does not
/// permit an invalid `LOCALHOLD_*` value to fall back to the configured default.
///
/// # Errors
///
/// Returns a configuration error when the file cannot be loaded or validated,
/// or when any recognized environment override is malformed.
pub fn load_effective_strict() -> Result<Config, EngineError> {
    let _stale_parse_warning = super::take_env_parse_warning();
    let loaded = Config::load();
    let malformed_override = super::take_env_parse_warning();
    if malformed_override {
        return Err(EngineError::config("at least one LOCALHOLD_* environment override is malformed; its value was ignored"));
    }
    loaded
}

const STARTER_CONFIG: &str = r#"# LocalHold configuration
# Complete reference: https://github.com/gearboxlogic/localhold/blob/main/localhold.example.toml
# Omitted settings use safe defaults and can be overridden with LOCALHOLD_* variables.

[database]
backend = "sqlite"

[embedding]
provider = "noop"

[server]
transport = "stdio"
"#;

/// Secret-free report of the platform configuration search policy.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ConfigPathsReport {
    /// Report contract version.
    pub schema_version: u32,
    /// Canonical path used by `config init`, when the platform exposes one.
    pub canonical_path: Option<String>,
    /// First existing configuration file in search order, if any.
    pub active_path: Option<String>,
    /// Paths searched in precedence order.
    pub searched_paths: Vec<String>,
}

impl ConfigPathsReport {
    /// Discover configuration paths for the current platform user.
    #[must_use]
    pub fn discover() -> Self {
        let config_dir = user_config_dir();
        Self::for_config_dir(config_dir.as_deref())
    }

    fn for_config_dir(config_dir: Option<&Path>) -> Self {
        let candidates = user_config_candidates(config_dir);
        let active_path = candidates.iter().find(|path| path.exists()).map(|path| display_path(path));
        Self {
            schema_version: REPORT_SCHEMA_VERSION,
            canonical_path: candidates.first().map(|path| display_path(path)),
            active_path,
            searched_paths: candidates.iter().map(|path| display_path(path)).collect(),
        }
    }

    /// Serialize the report as pretty JSON with a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the report cannot be encoded.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        pretty_json(self)
    }

    /// Render a concise human-readable report.
    #[must_use]
    pub fn render_text(&self) -> String {
        use std::fmt::Write as _;

        let canonical = single_line(self.canonical_path.as_deref().unwrap_or("unavailable"));
        let active = single_line(self.active_path.as_deref().unwrap_or("defaults (no config file found)"));
        let mut output = format!("Canonical config: {canonical}\nActive config: {active}\nSearched paths:\n");
        if self.searched_paths.is_empty() {
            output.push_str("  (platform config directory unavailable)\n");
        } else {
            for path in &self.searched_paths {
                let _written = writeln!(output, "  {}", single_line(path));
            }
        }
        output
    }
}

/// Outcome of validating the effective file and environment configuration.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ConfigValidationReport {
    /// Report contract version.
    pub schema_version: u32,
    /// Whether all configured values were loaded and validated.
    pub valid: bool,
    /// Process exit code corresponding to `valid`.
    pub exit_code: i32,
    /// Active configuration file, or `None` when defaults supplied the base.
    pub config_path: Option<String>,
    /// Secret-free operator summary.
    pub summary: String,
}

impl ConfigValidationReport {
    /// Validate configuration for the current platform user without opening storage,
    /// contacting providers, downloading models, or starting the server.
    #[must_use]
    pub fn validate() -> Self {
        let paths = ConfigPathsReport::discover();
        let _stale_parse_warning = super::take_env_parse_warning();
        let loaded = Config::load_with_source();
        let malformed_override = super::take_env_parse_warning();
        match loaded {
            Ok((_config, source)) if !malformed_override => {
                let config_path = source.as_deref().map(display_path);
                let summary = config_path.as_ref().map_or_else(
                    || "defaults and LOCALHOLD_* environment overrides are valid".to_owned(),
                    |path| format!("configuration file and LOCALHOLD_* environment overrides are valid at {path}"),
                );
                Self {
                    schema_version: REPORT_SCHEMA_VERSION,
                    valid: true,
                    exit_code: EXIT_VALID,
                    config_path,
                    summary,
                }
            }
            Ok((_config, source)) => Self::invalid(
                source.as_deref().map(display_path),
                "at least one LOCALHOLD_* environment override is malformed; its value was ignored",
            ),
            Err(_error) => Self::invalid(
                paths.active_path,
                "configuration could not be loaded or validated; secret-bearing parser context was suppressed",
            ),
        }
    }

    fn invalid(config_path: Option<String>, summary: impl Into<String>) -> Self {
        Self {
            schema_version: REPORT_SCHEMA_VERSION,
            valid: false,
            exit_code: EXIT_INVALID,
            config_path,
            summary: summary.into(),
        }
    }

    /// Serialize the report as pretty JSON with a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the report cannot be encoded.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        pretty_json(self)
    }

    /// Render a concise human-readable report.
    #[must_use]
    pub fn render_text(&self) -> String {
        let status = if self.valid { "valid" } else { "invalid" };
        let source = single_line(self.config_path.as_deref().unwrap_or("defaults (no config file found)"));
        format!("LocalHold config: {status}\nSource: {source}\nSummary: {}\n", single_line(&self.summary))
    }
}

/// Result of creating a starter configuration.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ConfigInitReport {
    /// Report contract version.
    pub schema_version: u32,
    /// Newly created configuration path.
    pub config_path: Option<String>,
    /// Whether the file was created.
    pub created: bool,
    /// Process exit code corresponding to `created`.
    pub exit_code: i32,
    /// Secret-free operator summary.
    pub summary: String,
}

impl ConfigInitReport {
    fn failed(config_path: Option<String>, summary: impl Into<String>) -> Self {
        Self {
            schema_version: REPORT_SCHEMA_VERSION,
            config_path,
            created: false,
            exit_code: EXIT_INVALID,
            summary: summary.into(),
        }
    }

    /// Serialize the report as pretty JSON with a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the report cannot be encoded.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        pretty_json(self)
    }

    /// Render a concise human-readable report.
    #[must_use]
    pub fn render_text(&self) -> String {
        if self.created {
            format!("Created LocalHold config at {}\n", single_line(self.config_path.as_deref().unwrap_or("unknown path")))
        } else {
            format!("LocalHold config was not created: {}\n", single_line(&self.summary))
        }
    }
}

/// Create a minimal starter configuration at the canonical platform path.
///
/// This operation never replaces an existing path. On Unix, the new file is
/// created with owner-only permissions before its contents are written.
///
/// Refusals and filesystem failures are returned as `created = false` reports
/// so `--json` callers always receive the documented schema.
#[must_use]
pub fn init() -> ConfigInitReport {
    let config_dir = user_config_dir();
    let candidates = user_config_candidates(config_dir.as_deref());
    let Some(path) = candidates.into_iter().next() else {
        return ConfigInitReport::failed(None, "platform user configuration directory is unavailable");
    };
    match init_at(&path) {
        Ok(created) => created,
        Err(error) => ConfigInitReport::failed(Some(display_path(&path)), error.to_string()),
    }
}

fn init_at(path: &Path) -> Result<ConfigInitReport, EngineError> {
    if path.exists() {
        return Err(EngineError::config(format!(
            "refusing to overwrite existing configuration at {}; edit or replace it explicitly",
            path.display()
        )));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| EngineError::config(format!("configuration path has no parent directory: {}", path.display())))?;
    std::fs::create_dir_all(parent).map_err(|error| EngineError::config(format!("creating {}: {error}", parent.display())))?;

    let mut options = OpenOptions::new();
    let _options = options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let _options = options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            EngineError::config(format!("refusing to overwrite existing configuration at {}; edit or replace it explicitly", path.display()))
        } else {
            EngineError::config(format!("creating {}: {error}", path.display()))
        }
    })?;
    if let Err(error) = file.write_all(STARTER_CONFIG.as_bytes()).and_then(|()| file.sync_all()) {
        drop(file);
        return match std::fs::remove_file(path) {
            Ok(()) => Err(EngineError::config(format!("writing {}: {error}", path.display()))),
            Err(cleanup_error) => Err(EngineError::config(format!(
                "writing {}: {error}; cleanup also failed and a partial file may remain at {}: {cleanup_error}",
                path.display(),
                path.display()
            ))),
        };
    }

    Ok(ConfigInitReport {
        schema_version: REPORT_SCHEMA_VERSION,
        config_path: Some(display_path(path)),
        created: true,
        exit_code: EXIT_VALID,
        summary: "minimal starter configuration created without replacing an existing path".to_owned(),
    })
}

fn pretty_json(value: &impl Serialize) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(value).map(|mut json| {
        json.push('\n');
        json
    })
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn single_line(value: &str) -> String {
    value.chars().map(|character| if character.is_control() { '\u{fffd}' } else { character }).collect()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("localhold-config-operator-{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn paths_report_canonical_and_active_file_without_loading_it() {
        let root = unique_temp_dir("paths");
        let path = root.join("localhold/localhold.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "secret invalid contents").unwrap();

        let report = ConfigPathsReport::for_config_dir(Some(&root));

        let expected = display_path(&path);
        assert_eq!(report.canonical_path.as_deref(), Some(expected.as_str()));
        assert_eq!(report.active_path.as_deref(), Some(expected.as_str()));
        assert_eq!(report.searched_paths, [expected]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn init_creates_valid_starter_and_never_clobbers_it() {
        let root = unique_temp_dir("init");
        let path = root.join("localhold/localhold.toml");

        let report = init_at(&path).unwrap();
        let original = fs::read_to_string(&path).unwrap();
        let parsed: Config = toml::from_str(&original).unwrap();
        let expected = display_path(&path);
        assert_eq!(report.config_path.as_deref(), Some(expected.as_str()));
        assert_eq!(parsed.database.backend, super::super::DatabaseBackend::Sqlite);

        let error = init_at(&path).unwrap_err().to_string();
        assert!(error.contains("refusing to overwrite"));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn init_uses_owner_only_file_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = unique_temp_dir("permissions");
        let path = root.join("localhold/localhold.toml");
        let _report = init_at(&path).unwrap();

        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn text_reports_flatten_control_characters() {
        let report = ConfigInitReport::failed(Some("/tmp/path\nforged".into()), "failed\r\nforged");
        let rendered = report.render_text();

        assert!(!rendered.contains("\nforged"));
        assert!(!rendered.contains('\r'));
    }
}
