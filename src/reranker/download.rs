//! Downloads ONNX model files from `HuggingFace` for cross-encoder reranking.
//!
//! Called once at startup when the model cache is empty. Uses blocking HTTP
//! requests (`reqwest::blocking`) because this module runs inside
//! `tokio::task::spawn_blocking` during one-time startup initialization.

use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Read, Write as _},
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};
use tracing::{info, warn};

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
/// Persistent lock file used to serialize cache validation and publication.
const CACHE_LOCK_FILE: &str = ".download.lock";

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

    let lock = open_cache_lock(&model_dir)?;
    lock.lock().map_err(|error| RerankerError::Transient(Box::new(error)))?;
    remove_stale_partial_files(&model_dir)?;

    for (remote_path, local_name) in MODEL_FILES {
        let local_path = model_dir.join(local_name);
        let expected_sha256 = match *local_name {
            "model.onnx" => pins.model_sha256.as_str(),
            "tokenizer.json" => pins.tokenizer_sha256.as_str(),
            other => return Err(RerankerError::Permanent(format!("unexpected reranker artifact '{other}'").into())),
        };
        let url = format!("{}/{}/resolve/{}/{remote_path}", download_base_url(), config.model, pins.revision);
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

    let (tmp_path, tmp_file) = create_temporary_download(dest)?;
    let result = (|| -> Result<u64, RerankerError> {
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

        publish_verified_file(&tmp_path, dest, expected_sha256)?;
        Ok(total_bytes)
    })();

    if result.is_err() {
        drop(std::fs::remove_file(&tmp_path));
    }

    let written = result?;
    info!("  saved {} ({} bytes, verified)", dest.display(), written);
    Ok(())
}

fn open_cache_lock(model_dir: &Path) -> Result<File, RerankerError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(model_dir.join(CACHE_LOCK_FILE))
        .map_err(|error| RerankerError::Permanent(Box::new(error)))
}

fn create_temporary_download(dest: &Path) -> Result<(PathBuf, File), RerankerError> {
    let file_name = dest.file_name().and_then(|name| name.to_str()).unwrap_or("download");
    for _ in 0..16_u8 {
        let path = dest.with_file_name(format!(".{file_name}.partial-{}-{:016x}", std::process::id(), fastrand::u64(..)));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(RerankerError::Permanent(Box::new(error))),
        }
    }
    Err(RerankerError::Transient("could not allocate a unique reranker download staging file".into()))
}

fn publish_verified_file(tmp_path: &Path, dest: &Path, expected_sha256: &str) -> Result<(), RerankerError> {
    match std::fs::rename(tmp_path, dest) {
        Ok(()) => Ok(()),
        Err(_error) if dest.exists() => {
            if verify_file_sha256(dest, expected_sha256)? {
                std::fs::remove_file(tmp_path).map_err(|remove_error| RerankerError::Permanent(Box::new(remove_error)))?;
                return Ok(());
            }
            // Windows does not replace an existing destination with rename.
            // The cache lock keeps cooperating readers out while the invalid
            // destination is removed and the verified staging file is moved.
            std::fs::remove_file(dest).map_err(|remove_error| RerankerError::Permanent(Box::new(remove_error)))?;
            std::fs::rename(tmp_path, dest).map_err(|rename_error| RerankerError::Permanent(Box::new(rename_error)))
        }
        Err(error) => Err(RerankerError::Permanent(Box::new(error))),
    }
}

fn remove_stale_partial_files(model_dir: &Path) -> Result<(), RerankerError> {
    let entries = std::fs::read_dir(model_dir).map_err(|error| RerankerError::Permanent(Box::new(error)))?;
    for entry in entries {
        let entry = entry.map_err(|error| RerankerError::Permanent(Box::new(error)))?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let is_partial = file_name == ".model.onnx.partial"
            || file_name == ".tokenizer.json.partial"
            || file_name.starts_with(".model.onnx.partial-")
            || file_name.starts_with(".tokenizer.json.partial-");
        if is_partial
            && let Err(error) = std::fs::remove_file(entry.path())
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!("could not remove stale reranker cache file {}: {error}", entry.path().display());
        }
    }
    Ok(())
}

fn download_base_url() -> String {
    #[cfg(test)]
    if let Ok(base_url) = std::env::var("LOCALHOLD_TEST_RERANKER_BASE_URL") {
        return base_url;
    }
    HF_BASE_URL.into()
}

// `expand_tilde` is in `crate::config::expand_tilde`.

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        net::{TcpListener, TcpStream},
        process::{Child, Command},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    use tempfile::TempDir;

    use super::*;

    const MODEL_BYTES: &[u8] = b"verified model artifact";
    const TOKENIZER_BYTES: &[u8] = b"verified tokenizer artifact";

    #[test]
    fn concurrent_processes_converge_on_one_verified_cache_entry() {
        let cache = TempDir::new().unwrap();
        let server = MockArtifactServer::start(false);
        let mut children = std::iter::repeat_with(|| spawn_download_child(cache.path(), &server.base_url)).take(4).collect::<Vec<_>>();

        for child in &mut children {
            let status = child.wait().unwrap();
            assert!(status.success(), "cache download child failed with {status}");
        }

        assert_eq!(server.request_count(), 2, "only one process should download the two artifacts");
        assert_verified_cache(cache.path());
    }

    #[test]
    fn failed_downloader_releases_lock_and_another_process_recovers() {
        let cache = TempDir::new().unwrap();
        seed_invalid_cache(cache.path());
        let server = MockArtifactServer::start(true);
        let mut children = std::iter::repeat_with(|| spawn_download_child(cache.path(), &server.base_url)).take(2).collect::<Vec<_>>();
        let statuses = children.iter_mut().map(|child| child.wait().unwrap()).collect::<Vec<_>>();

        assert_eq!(
            statuses.iter().filter(|status| status.success()).count(),
            1,
            "one process should recover after the injected failure"
        );
        assert_eq!(
            statuses.iter().filter(|status| !status.success()).count(),
            1,
            "the injected HTTP failure should reach one process"
        );
        assert_eq!(server.request_count(), 3, "the recovery process should retry the model and download the tokenizer");
        assert_verified_cache(cache.path());
    }

    #[test]
    fn cache_download_child_process() {
        let Ok(cache_dir) = std::env::var("LOCALHOLD_TEST_RERANKER_CACHE_CHILD") else {
            return;
        };
        let config = RerankerConfig {
            model: "test/model".into(),
            revision: "revision".into(),
            cache_dir,
            model_sha256: sha256_bytes(MODEL_BYTES),
            tokenizer_sha256: sha256_bytes(TOKENIZER_BYTES),
            ..RerankerConfig::default()
        };

        let paths = resolve_model_paths(&config).unwrap();
        assert_eq!(std::fs::read(paths.onnx_path).unwrap(), MODEL_BYTES);
        assert_eq!(std::fs::read(paths.tokenizer_path).unwrap(), TOKENIZER_BYTES);
    }

    fn spawn_download_child(cache_dir: &Path, base_url: &str) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "reranker::download::tests::cache_download_child_process", "--nocapture"])
            .env("LOCALHOLD_TEST_RERANKER_CACHE_CHILD", cache_dir)
            .env("LOCALHOLD_TEST_RERANKER_BASE_URL", base_url)
            .spawn()
            .unwrap()
    }

    fn assert_verified_cache(cache_dir: &Path) {
        let model_dir = cache_dir.join("test--model@revision");
        assert_eq!(std::fs::read(model_dir.join("model.onnx")).unwrap(), MODEL_BYTES);
        assert_eq!(std::fs::read(model_dir.join("tokenizer.json")).unwrap(), TOKENIZER_BYTES);
        let partials = std::fs::read_dir(model_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".partial"))
            .collect::<Vec<_>>();
        assert!(partials.is_empty(), "staging files should be cleaned up: {partials:?}");
    }

    fn seed_invalid_cache(cache_dir: &Path) {
        let model_dir = cache_dir.join("test--model@revision");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("model.onnx"), b"invalid model").unwrap();
        std::fs::write(model_dir.join("tokenizer.json"), b"invalid tokenizer").unwrap();
        std::fs::write(model_dir.join(".model.onnx.partial"), b"interrupted legacy download").unwrap();
        std::fs::write(model_dir.join(".tokenizer.json.partial-123-stale"), b"interrupted download").unwrap();
    }

    fn sha256_bytes(bytes: &[u8]) -> String {
        hex_lower(Sha256::digest(bytes))
    }

    struct MockArtifactServer {
        base_url: String,
        request_count: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl MockArtifactServer {
        fn start(fail_first: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let request_count = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let thread_request_count = Arc::clone(&request_count);
            let thread_stop = Arc::clone(&stop);
            let fail_next = AtomicBool::new(fail_first);
            let thread = thread::spawn(move || run_mock_server(&listener, &thread_request_count, &thread_stop, &fail_next));
            Self {
                base_url: format!("http://{address}"),
                request_count,
                stop,
                thread: Some(thread),
            }
        }

        fn request_count(&self) -> usize {
            self.request_count.load(Ordering::Acquire)
        }
    }

    impl Drop for MockArtifactServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    fn run_mock_server(listener: &TcpListener, request_count: &AtomicUsize, stop: &AtomicBool, fail_next: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => serve_artifact(stream, request_count, fail_next),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(5)),
                Err(_error) => return,
            }
        }
    }

    fn serve_artifact(mut stream: TcpStream, request_count: &AtomicUsize, fail_next: &AtomicBool) {
        let mut request = [0_u8; 2048];
        let read = stream.read(&mut request).unwrap();
        let _previous_request_count = request_count.fetch_add(1, Ordering::AcqRel);
        if fail_next.swap(false, Ordering::AcqRel) {
            stream
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
            return;
        }
        let request = String::from_utf8_lossy(&request[..read]);
        let body = if request.contains("tokenizer.json") { TOKENIZER_BYTES } else { MODEL_BYTES };
        let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        stream.write_all(response.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
    }
}
