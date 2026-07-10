//! Downloads ONNX model files from `HuggingFace` for cross-encoder reranking.
//!
//! Called once at startup when the model cache is empty. Uses blocking HTTP
//! requests (`reqwest::blocking`) because this module runs inside
//! `tokio::task::spawn_blocking` during one-time startup initialization.

use std::{
    fs::File,
    io::{BufWriter, Read, Write as _},
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};
use tracing::info;

use super::RerankerError;
use crate::config::{DEFAULT_RERANKER_REVISION, RerankerConfig, is_builtin_default_reranker_model};

/// Base URL template for `HuggingFace` model file downloads.
const HF_BASE_URL: &str = "https://huggingface.co";
/// SHA-256 of the pinned default `model.onnx`.
const DEFAULT_MODEL_SHA256: &str = "5d3e70fd0c9ff14b9b5169a51e957b7a9c74897afd0a35ce4bd318150c1d4d4a";
/// SHA-256 of the pinned default `tokenizer.json`.
const DEFAULT_TOKENIZER_SHA256: &str = "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66";
/// Hard cap for auto-downloaded model artifacts.
const MAX_DOWNLOAD_BYTES: u64 = 500 * 1024 * 1024;
/// Buffered copy chunk size for blocking downloads and hashing.
const COPY_BUFFER_BYTES: usize = 16 * 1024;
/// HTTP timeout for model downloads — generous to allow large files over slow connections.
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(5);

fn hex_lower(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut hex = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        hex.push(char::from(HEX[usize::from(byte >> 4_u8)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f_u8)]));
    }
    hex
}

/// Files required for ONNX cross-encoder inference.
const MODEL_FILES: &[(&str, &str)] = &[("onnx/model.onnx", "model.onnx"), ("tokenizer.json", "tokenizer.json")];

/// Resolved paths to the ONNX model and tokenizer files on disk.
#[derive(Debug)]
#[non_exhaustive]
pub(crate) struct ModelPaths {
    /// Path to the ONNX model file (`model.onnx`).
    pub onnx_path: PathBuf,
    /// Path to the tokenizer configuration (`tokenizer.json`).
    pub tokenizer_path: PathBuf,
}

#[derive(Debug, Clone)]
struct DownloadPins {
    revision: String,
    model_sha256: String,
    tokenizer_sha256: String,
}

/// Resolve (and download if necessary) the ONNX model files.
///
/// If `model_path` is non-empty, it is used as the direct path to the ONNX
/// model file and the tokenizer is expected to be in the same directory.
/// Otherwise, the model is looked up in `cache_dir/{model_name}/` and
/// downloaded from `HuggingFace` if not found.
pub(crate) fn resolve_model_paths(config: &RerankerConfig) -> Result<ModelPaths, RerankerError> {
    // If a direct model_path is specified, use it (expanding ~ like other paths)
    if !config.model_path.is_empty() {
        let onnx_path = crate::config::expand_tilde(&config.model_path).map_err(|e| RerankerError::Permanent(e.to_string().into()))?;
        let parent = onnx_path.parent().unwrap_or_else(|| Path::new("."));
        let tokenizer_path = parent.join("tokenizer.json");
        return Ok(ModelPaths { onnx_path, tokenizer_path });
    }

    let pins = resolve_download_pins(config)?;

    // Expand ~ in cache_dir
    let expanded_cache = crate::config::expand_tilde(&config.cache_dir).map_err(|e| RerankerError::Permanent(e.to_string().into()))?;
    let model_name = config.model.replace('/', "--");
    let revision_name = pins.revision.replace('/', "--");
    let model_dir = expanded_cache.join(format!("{model_name}@{revision_name}"));

    let onnx_path = model_dir.join("model.onnx");
    let tokenizer_path = model_dir.join("tokenizer.json");

    // Download missing files
    info!("resolving reranker model '{}' at revision {} in {}", config.model, pins.revision, model_dir.display());
    std::fs::create_dir_all(&model_dir).map_err(|e| RerankerError::Permanent(Box::new(e)))?;

    for (remote_path, local_name) in MODEL_FILES {
        let local_path = model_dir.join(local_name);
        let expected_sha256 = match *local_name {
            "model.onnx" => pins.model_sha256.as_str(),
            "tokenizer.json" => pins.tokenizer_sha256.as_str(),
            other => return Err(RerankerError::Permanent(format!("unexpected reranker artifact '{other}'").into())),
        };
        let url = format!("{HF_BASE_URL}/{}/resolve/{}/{remote_path}", config.model, pins.revision);
        ensure_cached_file(&url, &local_path, expected_sha256)?;
    }

    Ok(ModelPaths { onnx_path, tokenizer_path })
}

fn resolve_download_pins(config: &RerankerConfig) -> Result<DownloadPins, RerankerError> {
    if is_builtin_default_reranker_model(&config.model) {
        // Overriding the revision on the builtin model requires explicit
        // hashes — the pinned defaults only match the pinned revision.
        let custom_revision = !config.revision.is_empty() && config.revision != DEFAULT_RERANKER_REVISION;
        if custom_revision && (config.model_sha256.is_empty() || config.tokenizer_sha256.is_empty()) {
            return Err(RerankerError::Permanent(
                "overriding revision on the builtin reranker model requires explicit model_sha256 and tokenizer_sha256".into(),
            ));
        }

        return Ok(DownloadPins {
            revision: if config.revision.is_empty() {
                DEFAULT_RERANKER_REVISION.into()
            } else {
                config.revision.clone()
            },
            model_sha256: if config.model_sha256.is_empty() {
                DEFAULT_MODEL_SHA256.into()
            } else {
                config.model_sha256.clone()
            },
            tokenizer_sha256: if config.tokenizer_sha256.is_empty() {
                DEFAULT_TOKENIZER_SHA256.into()
            } else {
                config.tokenizer_sha256.clone()
            },
        });
    }

    if config.revision.is_empty() {
        return Err(RerankerError::Permanent("reranker.revision must be set for custom auto-downloaded models".into()));
    }
    if config.model_sha256.is_empty() {
        return Err(RerankerError::Permanent("reranker.model_sha256 must be set for custom auto-downloaded models".into()));
    }
    if config.tokenizer_sha256.is_empty() {
        return Err(RerankerError::Permanent("reranker.tokenizer_sha256 must be set for custom auto-downloaded models".into()));
    }

    Ok(DownloadPins {
        revision: config.revision.clone(),
        model_sha256: config.model_sha256.clone(),
        tokenizer_sha256: config.tokenizer_sha256.clone(),
    })
}

fn ensure_cached_file(url: &str, dest: &Path, expected_sha256: &str) -> Result<(), RerankerError> {
    if dest.exists() && verify_file_sha256(dest, expected_sha256)? {
        info!("  {} already cached and verified", dest.display());
        return Ok(());
    }

    if dest.exists() {
        info!("  {} cached but hash mismatch, re-downloading", dest.display());
        std::fs::remove_file(dest).map_err(|e| RerankerError::Permanent(Box::new(e)))?;
    }

    download_file(url, dest, expected_sha256)
}

fn verify_file_sha256(path: &Path, expected_sha256: &str) -> Result<bool, RerankerError> {
    let mut file = File::open(path).map_err(|e| RerankerError::Permanent(Box::new(e)))?;
    let actual_sha256 = sha256_reader(&mut file)?;
    Ok(actual_sha256.eq_ignore_ascii_case(expected_sha256))
}

fn sha256_reader<R: Read>(reader: &mut R) -> Result<String, RerankerError> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer).map_err(|e| RerankerError::Transient(Box::new(e)))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_lower(hasher.finalize()))
}

/// Download a single file from `url` to `dest` using blocking HTTP.
fn download_file(url: &str, dest: &Path, expected_sha256: &str) -> Result<(), RerankerError> {
    info!("  downloading {}", url);
    let client = reqwest::blocking::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|e| RerankerError::Permanent(Box::new(e)))?;
    let mut response = client.get(url).send().map_err(|e| RerankerError::Transient(Box::new(e)))?;

    if !response.status().is_success() {
        return Err(RerankerError::Permanent(format!("HTTP {} downloading {url}", response.status()).into()));
    }

    if response.content_length().is_some_and(|len| len > MAX_DOWNLOAD_BYTES) {
        return Err(RerankerError::Permanent(
            format!(
                "download too large: {} bytes exceeds {}",
                response.content_length().unwrap_or(MAX_DOWNLOAD_BYTES),
                MAX_DOWNLOAD_BYTES
            )
            .into(),
        ));
    }

    let tmp_path = temporary_download_path(dest);
    let result = (|| -> Result<u64, RerankerError> {
        let tmp_file = File::create(&tmp_path).map_err(|e| RerankerError::Permanent(Box::new(e)))?;
        let mut writer = BufWriter::new(tmp_file);
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];
        let mut total_bytes = 0_u64;

        loop {
            let read = response.read(&mut buffer).map_err(|e| RerankerError::Transient(Box::new(e)))?;
            if read == 0 {
                break;
            }
            total_bytes = total_bytes
                .checked_add(u64::try_from(read).map_err(|e| RerankerError::Permanent(Box::new(e)))?)
                .ok_or_else(|| RerankerError::Permanent("download size overflow".into()))?;
            if total_bytes > MAX_DOWNLOAD_BYTES {
                return Err(RerankerError::Permanent(
                    format!("download too large: {total_bytes} bytes exceeds {MAX_DOWNLOAD_BYTES}").into(),
                ));
            }

            hasher.update(&buffer[..read]);
            writer.write_all(&buffer[..read]).map_err(|e| RerankerError::Permanent(Box::new(e)))?;
        }

        writer.flush().map_err(|e| RerankerError::Permanent(Box::new(e)))?;
        writer.get_ref().sync_all().map_err(|e| RerankerError::Permanent(Box::new(e)))?;

        let actual_sha256 = hex_lower(hasher.finalize());
        if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
            return Err(RerankerError::Permanent(
                format!("download hash mismatch for {}: expected {expected_sha256}, got {actual_sha256}", dest.display()).into(),
            ));
        }

        std::fs::rename(&tmp_path, dest).map_err(|e| RerankerError::Permanent(Box::new(e)))?;
        Ok(total_bytes)
    })();

    if result.is_err() {
        drop(std::fs::remove_file(&tmp_path));
    }

    let written = result?;
    info!("  saved {} ({} bytes, verified)", dest.display(), written);
    Ok(())
}

fn temporary_download_path(dest: &Path) -> PathBuf {
    let file_name = dest.file_name().and_then(|name| name.to_str()).unwrap_or("download");
    dest.with_file_name(format!(".{file_name}.partial"))
}

// `expand_tilde` is in `crate::config::expand_tilde`.
