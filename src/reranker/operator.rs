//! Operator-facing reranker artifact verification and download reports.

use std::fmt::Write as _;

use serde::Serialize;

use super::{
    RerankerError, download,
    download::{ArtifactStatus, ModelInspection},
    runtime::{RerankerModelIdentity, model_identity},
};
use crate::config::RerankerConfig;

/// Stable schema version for `hold models` JSON output.
pub const MODELS_REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy)]
struct ReportOptions<'a> {
    command: &'a str,
    network_allowed: bool,
    artifacts_changed: bool,
}

/// Verification state for one model artifact.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ModelArtifactReport {
    /// Artifact role (`model` or `tokenizer`).
    pub name: String,
    /// Resolved local filesystem path.
    pub path: String,
    /// Configured immutable SHA-256, or `null` when a direct file is unpinned.
    pub expected_sha256: Option<String>,
    /// Offline verification result.
    pub status: String,
}

/// Versioned report returned by `hold models verify` and `hold models fetch`.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ModelsReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Operation that produced the report.
    pub command: String,
    /// Configured model identifier.
    pub model: String,
    /// Immutable revision or `direct_file` marker.
    pub revision: String,
    /// Managed artifact profile (`fused`, `custom`, or `direct_file`).
    pub artifact: String,
    /// Configured numeric precision.
    pub precision: String,
    /// Storage source inspected by the command.
    pub source: String,
    /// Whether this invocation was permitted to use the network.
    pub network_allowed: bool,
    /// Whether the configured artifact set changed from an unverified state to verified.
    pub artifacts_changed: bool,
    /// Aggregate operation state.
    pub status: String,
    /// Per-file verification details.
    pub artifacts: Vec<ModelArtifactReport>,
    /// Human-readable, single-line result summary.
    pub summary: String,
    /// Process exit code associated with the report.
    pub exit_code: i32,
}

impl ModelsReport {
    /// Return a structured refusal when a network-capable fetch lacks `--yes`.
    #[must_use]
    pub fn confirmation_refused() -> Self {
        Self {
            schema_version: MODELS_REPORT_SCHEMA_VERSION,
            command: "fetch".into(),
            model: "not_loaded".into(),
            revision: "not_loaded".into(),
            artifact: "not_loaded".into(),
            precision: "not_loaded".into(),
            source: "not_loaded".into(),
            network_allowed: false,
            artifacts_changed: false,
            status: "refused".into(),
            artifacts: Vec::new(),
            summary: "refusing network-capable model fetch without explicit --yes confirmation".into(),
            exit_code: 1,
        }
    }

    /// Serialize the stable report schema as one JSON line.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the report cannot be encoded.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self).map(|mut json| {
            json.push('\n');
            json
        })
    }

    /// Render a concise operator-facing text report.
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut output = format!(
            "models {}: {}\nmodel: {}\nrevision: {}\nartifact: {}\nprecision: {}\nsource: {}\nnetwork allowed: {}\nartifacts changed: {}\n",
            clean_text(&self.command),
            clean_text(&self.status),
            clean_text(&self.model),
            clean_text(&self.revision),
            clean_text(&self.artifact),
            clean_text(&self.precision),
            clean_text(&self.source),
            self.network_allowed,
            self.artifacts_changed,
        );
        for artifact in &self.artifacts {
            let expected = artifact.expected_sha256.as_deref().unwrap_or("not configured");
            let _written = writeln!(
                output,
                "{}: {} ({}, sha256 {})",
                clean_text(&artifact.name),
                clean_text(&artifact.status),
                clean_text(&artifact.path),
                clean_text(expected),
            );
        }
        let _written = writeln!(output, "summary: {}", clean_text(&self.summary));
        output
    }
}

/// Inspect configured reranker artifacts without creating paths or using the network.
#[must_use]
pub fn verify(config: &RerankerConfig) -> ModelsReport {
    let identity = match model_identity(config) {
        Ok(identity) => identity,
        Err(error) => return report_from_error("verify", config, false, None, &error),
    };
    match download::inspect_model_paths(config) {
        Ok(inspection) => report_from_inspection(
            ReportOptions {
                command: "verify",
                network_allowed: false,
                artifacts_changed: false,
            },
            config,
            &identity,
            &inspection,
        ),
        Err(error) => report_from_error("verify", config, false, Some(&identity), &error),
    }
}

/// Fetch missing or invalid managed artifacts, then verify both SHA-256 pins.
///
/// Direct-file configurations are never downloaded; they are verified in place.
#[must_use]
pub fn fetch(config: &RerankerConfig) -> ModelsReport {
    let identity = match model_identity(config) {
        Ok(identity) => identity,
        Err(error) => return report_from_error("fetch", config, true, None, &error),
    };
    let before = match download::inspect_model_paths(config) {
        Ok(before) => before,
        Err(error) => return report_from_error("fetch", config, true, Some(&identity), &error),
    };
    if !config.model_path.is_empty() {
        return report_from_inspection(
            ReportOptions {
                command: "fetch",
                network_allowed: true,
                artifacts_changed: false,
            },
            config,
            &identity,
            &before,
        );
    }
    let artifacts_changed = !before.is_verified();
    match download::resolve_model_paths(config) {
        Ok(_paths) => match download::inspect_model_paths(config) {
            Ok(inspection) => report_from_inspection(
                ReportOptions {
                    command: "fetch",
                    network_allowed: true,
                    artifacts_changed,
                },
                config,
                &identity,
                &inspection,
            ),
            Err(error) => report_from_error("fetch", config, true, Some(&identity), &error),
        },
        Err(error) => report_from_error("fetch", config, true, Some(&identity), &error),
    }
}

fn report_from_inspection(options: ReportOptions<'_>, config: &RerankerConfig, identity: &RerankerModelIdentity, inspection: &ModelInspection) -> ModelsReport {
    let (revision, artifact, precision) = identity_fields(config, Some(identity));
    let aggregate = aggregate_status(inspection.model_status, inspection.tokenizer_status);
    let exit_code = i32::from(aggregate != "verified");
    ModelsReport {
        schema_version: MODELS_REPORT_SCHEMA_VERSION,
        command: options.command.into(),
        model: config.model.clone(),
        revision,
        artifact,
        precision,
        source: if config.model_path.is_empty() { "managed_cache".into() } else { "direct_file".into() },
        network_allowed: options.network_allowed,
        artifacts_changed: options.artifacts_changed && aggregate == "verified",
        status: aggregate.into(),
        artifacts: vec![
            artifact_report("model", &inspection.paths.onnx_path, &inspection.model_sha256, inspection.model_status),
            artifact_report("tokenizer", &inspection.paths.tokenizer_path, &inspection.tokenizer_sha256, inspection.tokenizer_status),
        ],
        summary: summary_for(aggregate, config.model_path.is_empty(), options.command),
        exit_code,
    }
}

fn report_from_error(command: &str, config: &RerankerConfig, network_allowed: bool, identity: Option<&RerankerModelIdentity>, error: &RerankerError) -> ModelsReport {
    let (revision, artifact, precision) = identity_fields(config, identity);
    ModelsReport {
        schema_version: MODELS_REPORT_SCHEMA_VERSION,
        command: command.into(),
        model: config.model.clone(),
        revision,
        artifact,
        precision,
        source: if config.model_path.is_empty() { "managed_cache".into() } else { "direct_file".into() },
        network_allowed,
        artifacts_changed: false,
        status: "error".into(),
        artifacts: Vec::new(),
        summary: format!("model operation failed: {error}"),
        exit_code: 1,
    }
}

fn identity_fields(config: &RerankerConfig, identity: Option<&RerankerModelIdentity>) -> (String, String, String) {
    identity.map_or_else(
        || {
            (
                if config.revision.is_empty() { "not_resolved".into() } else { config.revision.clone() },
                if config.model_path.is_empty() { "not_resolved".into() } else { "direct_file".into() },
                config.precision.to_string(),
            )
        },
        |identity| (identity.revision.clone(), identity.artifact.clone(), identity.precision.to_string()),
    )
}

fn artifact_report(name: &str, path: &std::path::Path, expected_sha256: &str, status: ArtifactStatus) -> ModelArtifactReport {
    ModelArtifactReport {
        name: name.into(),
        path: path.to_string_lossy().into_owned(),
        expected_sha256: (!expected_sha256.is_empty()).then(|| expected_sha256.to_owned()),
        status: status_name(status).into(),
    }
}

const fn aggregate_status(model: ArtifactStatus, tokenizer: ArtifactStatus) -> &'static str {
    if matches!(model, ArtifactStatus::Missing) || matches!(tokenizer, ArtifactStatus::Missing) {
        "missing"
    } else if matches!(model, ArtifactStatus::HashMismatch) || matches!(tokenizer, ArtifactStatus::HashMismatch) {
        "hash_mismatch"
    } else if matches!(model, ArtifactStatus::Unverifiable) || matches!(tokenizer, ArtifactStatus::Unverifiable) {
        "unverifiable"
    } else {
        "verified"
    }
}

const fn status_name(status: ArtifactStatus) -> &'static str {
    match status {
        ArtifactStatus::Verified => "verified",
        ArtifactStatus::Missing => "missing",
        ArtifactStatus::HashMismatch => "hash_mismatch",
        ArtifactStatus::Unverifiable => "unverifiable",
    }
}

fn summary_for(status: &str, managed: bool, command: &str) -> String {
    match status {
        "verified" if command == "fetch" && managed => "managed reranker artifacts are present and match both configured SHA-256 hashes".into(),
        "verified" => "reranker model and tokenizer match both configured SHA-256 hashes".into(),
        "missing" => "one or more configured reranker artifacts are missing".into(),
        "hash_mismatch" => "one or more reranker artifacts do not match their configured SHA-256 hashes".into(),
        "unverifiable" => "direct reranker files require both model_sha256 and tokenizer_sha256 pins for offline verification".into(),
        _ => "reranker artifact verification failed".into(),
    }
}

fn clean_text(value: &str) -> String {
    value.chars().map(|character| if character.is_control() { ' ' } else { character }).collect()
}

#[cfg(test)]
mod tests {
    use sha2::{Digest as _, Sha256};
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn direct_files_require_pins_and_verify_both_hashes() {
        let root = TempDir::new().unwrap();
        let model_path = root.path().join("model.onnx");
        let tokenizer_path = root.path().join("tokenizer.json");
        std::fs::write(&model_path, b"model").unwrap();
        std::fs::write(&tokenizer_path, b"tokenizer").unwrap();
        let mut config = RerankerConfig {
            model_path: model_path.to_string_lossy().into_owned(),
            ..RerankerConfig::default()
        };

        let unpinned = verify(&config);
        assert_eq!(unpinned.status, "unverifiable");
        assert_eq!(unpinned.exit_code, 1_i32);

        config.model_sha256 = hex(Sha256::digest(b"model"));
        config.tokenizer_sha256 = hex(Sha256::digest(b"tokenizer"));
        let verified = verify(&config);
        assert_eq!(verified.status, "verified");
        assert_eq!(verified.exit_code, 0_i32);

        std::fs::write(tokenizer_path, b"tampered").unwrap();
        let mismatch = verify(&config);
        assert_eq!(mismatch.status, "hash_mismatch");
        assert_eq!(mismatch.artifacts[1].status, "hash_mismatch");
    }

    #[test]
    fn verification_of_missing_managed_cache_is_offline_and_read_only() {
        let root = TempDir::new().unwrap();
        let cache = root.path().join("must-not-be-created");
        let config = RerankerConfig {
            cache_dir: cache.to_string_lossy().into_owned(),
            ..RerankerConfig::default()
        };

        let report = verify(&config);

        assert_eq!(report.status, "missing");
        assert!(!report.network_allowed);
        assert!(!cache.exists());
    }

    #[test]
    fn aggregate_status_prioritizes_missing_artifacts_over_hash_mismatches() {
        assert_eq!(aggregate_status(ArtifactStatus::HashMismatch, ArtifactStatus::Missing), "missing");
        assert_eq!(aggregate_status(ArtifactStatus::Missing, ArtifactStatus::HashMismatch), "missing");
    }

    fn hex(bytes: impl AsRef<[u8]>) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(bytes.as_ref().len().saturating_mul(2));
        for byte in bytes.as_ref() {
            output.push(char::from(HEX[usize::from(byte >> 4_u8)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f_u8)]));
        }
        output
    }
}
