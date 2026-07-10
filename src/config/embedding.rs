//! Embedding provider configuration, normalization, and validation.

use std::{collections::HashMap, fmt, net::IpAddr, str::FromStr};

use serde::{Deserialize, Serialize};

use super::apply_parsed_env;
use crate::error::{EngineError, ParseEnumError};

/// Default dimensionality used when embeddings are enabled.
pub const DEFAULT_EMBEDDING_DIMENSIONS: usize = 768;

/// Authentication header used by an OpenAI-compatible endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OpenAiAuthMode {
    /// Send the configured key as `Authorization: Bearer <key>`.
    #[default]
    Bearer,
    /// Send the configured key in the Azure-compatible `api-key` header.
    ApiKey,
}

impl fmt::Display for OpenAiAuthMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer => f.write_str("bearer"),
            Self::ApiKey => f.write_str("api_key"),
        }
    }
}

impl FromStr for OpenAiAuthMode {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "bearer" => Ok(Self::Bearer),
            "api_key" => Ok(Self::ApiKey),
            other => Err(ParseEnumError(format!("unknown OpenAI-compatible auth mode {other:?}, expected \"bearer\" or \"api_key\""))),
        }
    }
}

/// Startup and recovery health-check behavior for an embedding endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EmbeddingHealthCheck {
    /// Require `GET /models` to list the configured model.
    #[default]
    Models,
    /// Assume availability until an embedding request fails.
    Disabled,
}

impl fmt::Display for EmbeddingHealthCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Models => f.write_str("models"),
            Self::Disabled => f.write_str("disabled"),
        }
    }
}

impl FromStr for EmbeddingHealthCheck {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "models" => Ok(Self::Models),
            "disabled" => Ok(Self::Disabled),
            other => Err(ParseEnumError(format!("unknown embedding health check {other:?}, expected \"models\" or \"disabled\""))),
        }
    }
}

/// Provider-agnostic embedding configuration.
///
/// Uses a tagged union so provider-specific fields remain isolated as new
/// providers are added.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EmbeddingConfig {
    /// OpenAI-compatible embedding provider.
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible {
        /// Dimensionality of the embedding vectors.
        #[serde(default = "default_embedding_dimensions")]
        dimensions: usize,
        /// OpenAI-compatible endpoint parameters.
        #[serde(default)]
        openai_compatible: OpenAiCompatibleConfig,
    },
    /// No-op provider; memories are stored without embeddings.
    Noop {
        /// Dimensionality used for schema initialization.
        #[serde(default = "default_embedding_dimensions")]
        dimensions: usize,
    },
}

const fn default_embedding_dimensions() -> usize {
    DEFAULT_EMBEDDING_DIMENSIONS
}

/// OpenAI-compatible embedding endpoint parameters.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct OpenAiCompatibleConfig {
    /// Base URL for the API, including the version path when required.
    pub base_url: String,
    /// Embedding model name or deployment identifier.
    pub model: String,
    /// Optional endpoint credential.
    pub api_key: Option<String>,
    /// Header convention used when `api_key` is configured.
    pub auth_mode: OpenAiAuthMode,
    /// Whether to include the configured vector size in embedding requests.
    pub send_dimensions: bool,
    /// Health-check strategy used during startup and outage recovery.
    pub health_check: EmbeddingHealthCheck,
    /// Permit unencrypted HTTP for a non-loopback endpoint.
    pub allow_insecure_http: bool,
}

impl fmt::Debug for OpenAiCompatibleConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompatibleConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("auth_mode", &self.auth_mode)
            .field("send_dimensions", &self.send_dimensions)
            .field("health_check", &self.health_check)
            .field("allow_insecure_http", &self.allow_insecure_http)
            .finish()
    }
}

impl Default for OpenAiCompatibleConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:8000/v1".into(),
            model: "nomic-embed-text".into(),
            api_key: None,
            auth_mode: OpenAiAuthMode::default(),
            send_dimensions: false,
            health_check: EmbeddingHealthCheck::default(),
            allow_insecure_http: false,
        }
    }
}

impl EmbeddingConfig {
    /// Embedding vector dimensionality, regardless of provider.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        match self {
            Self::OpenAiCompatible { dimensions, .. } | Self::Noop { dimensions, .. } => *dimensions,
        }
    }

    /// OpenAI-compatible endpoint parameters, if that provider is active.
    #[must_use]
    pub const fn openai_compatible(&self) -> Option<&OpenAiCompatibleConfig> {
        match self {
            Self::OpenAiCompatible { openai_compatible, .. } => Some(openai_compatible),
            Self::Noop { .. } => None,
        }
    }
}

pub(super) fn normalize_embedding_config(config: &mut EmbeddingConfig) {
    if let EmbeddingConfig::OpenAiCompatible { openai_compatible, .. } = config {
        openai_compatible.base_url = openai_compatible.base_url.trim().trim_end_matches('/').to_owned();
        openai_compatible.model = openai_compatible.model.trim().to_owned();
        openai_compatible.api_key = openai_compatible
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|api_key| !api_key.is_empty())
            .map(ToOwned::to_owned);
    }
}

pub(super) fn validate_embedding_config(config: &EmbeddingConfig) -> Result<(), EngineError> {
    if config.dimensions() == 0 {
        return Err(EngineError::config("embedding dimensions must be greater than zero"));
    }

    if let EmbeddingConfig::OpenAiCompatible { openai_compatible, .. } = config {
        validate_openai_compatible_config(openai_compatible)?;
    }
    Ok(())
}

pub(super) fn validate_openai_compatible_config(config: &OpenAiCompatibleConfig) -> Result<(), EngineError> {
    let base_url = config.base_url.trim();
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return Err(EngineError::config("embedding.openai_compatible.base_url must start with http:// or https://"));
    }

    let parsed = reqwest::Url::parse(base_url).map_err(|error| EngineError::config(format!("embedding.openai_compatible.base_url is invalid: {error}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| EngineError::config("embedding.openai_compatible.base_url must include a host"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(EngineError::config(
            "embedding.openai_compatible.base_url must not contain credentials; use embedding.openai_compatible.api_key",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(EngineError::config("embedding.openai_compatible.base_url must not contain a query string or fragment"));
    }
    if parsed.scheme() == "http" && !config.allow_insecure_http && !is_loopback_host(host) {
        return Err(EngineError::config(
            "embedding.openai_compatible.base_url must use https for non-loopback hosts; set allow_insecure_http = true only for a trusted network",
        ));
    }
    if config.model.trim().is_empty() {
        return Err(EngineError::config("embedding.openai_compatible.model must not be empty"));
    }
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|address| address.is_loopback())
}

pub(super) fn apply_embedding_env(config: &mut EmbeddingConfig, env: &HashMap<String, String>) {
    const PROVIDER_KEYS: [&str; 7] = [
        "LOCALHOLD_EMBEDDING_BASE_URL",
        "LOCALHOLD_EMBEDDING_MODEL",
        "LOCALHOLD_EMBEDDING_API_KEY",
        "LOCALHOLD_EMBEDDING_AUTH_MODE",
        "LOCALHOLD_EMBEDDING_SEND_DIMENSIONS",
        "LOCALHOLD_EMBEDDING_HEALTH_CHECK",
        "LOCALHOLD_EMBEDDING_ALLOW_INSECURE_HTTP",
    ];
    if PROVIDER_KEYS.iter().any(|key| env.contains_key(*key)) && matches!(config, EmbeddingConfig::Noop { .. }) {
        let dimensions = config.dimensions();
        *config = EmbeddingConfig::OpenAiCompatible {
            dimensions,
            openai_compatible: OpenAiCompatibleConfig::default(),
        };
    }

    match config {
        EmbeddingConfig::OpenAiCompatible { dimensions, openai_compatible } => {
            if let Some(value) = env.get("LOCALHOLD_EMBEDDING_BASE_URL") {
                openai_compatible.base_url.clone_from(value);
            }
            if let Some(value) = env.get("LOCALHOLD_EMBEDDING_MODEL") {
                openai_compatible.model.clone_from(value);
            }
            if let Some(value) = env.get("LOCALHOLD_EMBEDDING_API_KEY") {
                openai_compatible.api_key = Some(value.clone());
            }
            apply_parsed_env(env, "LOCALHOLD_EMBEDDING_AUTH_MODE", &mut openai_compatible.auth_mode);
            apply_parsed_env(env, "LOCALHOLD_EMBEDDING_SEND_DIMENSIONS", &mut openai_compatible.send_dimensions);
            apply_parsed_env(env, "LOCALHOLD_EMBEDDING_HEALTH_CHECK", &mut openai_compatible.health_check);
            apply_parsed_env(env, "LOCALHOLD_EMBEDDING_ALLOW_INSECURE_HTTP", &mut openai_compatible.allow_insecure_http);
            apply_parsed_env(env, "LOCALHOLD_EMBEDDING_DIMENSIONS", dimensions);
        }
        EmbeddingConfig::Noop { dimensions } => apply_parsed_env(env, "LOCALHOLD_EMBEDDING_DIMENSIONS", dimensions),
    }
}
