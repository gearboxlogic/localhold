//! ONNX cross-encoder reranker using `ort` for inference and `tokenizers` for tokenization.
//!
//! Scores (query, document) pairs by running a cross-encoder model (e.g.
//! `ms-marco-MiniLM-L-6-v2`) entirely in-process. The model is loaded from
//! a local cache or downloaded from `HuggingFace` on first use.

use std::sync::Arc;

#[cfg(feature = "reranker-cuda")]
use ort::execution_providers::CUDAExecutionProvider;
use ort::{session::Session, value::Tensor};
use parking_lot::Mutex;
use tracing::info;

use super::{BoxFuture, RerankerError, RerankerProvider, RerankerScore, download};
use crate::config::RerankerConfig;

/// Cross-encoder reranker backed by an ONNX Runtime session.
///
/// The ONNX [`Session`] requires `&mut self` for `run()`, so we wrap it in a
/// [`Mutex`] to satisfy the `Send + Sync` requirement of [`RerankerProvider`].
/// Since reranking is CPU-bound and brief, contention is minimal.
pub struct OnnxReranker {
    inner: Arc<OnnxRerankerInner>,
}

struct OnnxRerankerInner {
    session: Mutex<Session>,
    tokenizer: tokenizers::Tokenizer,
    /// Whether the ONNX graph declares a `token_type_ids` input.
    /// BERT-style models need it; RoBERTa/DistilBERT-style models do not.
    has_token_type_ids: bool,
}

impl std::fmt::Debug for OnnxReranker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxReranker").finish_non_exhaustive()
    }
}

impl OnnxReranker {
    /// Default maximum sequence length for cross-encoder tokenization.
    /// Matches the positional embedding limit of `ms-marco-MiniLM-L-6-v2`.
    const DEFAULT_MAX_LENGTH: usize = 512;

    /// Load an ONNX cross-encoder model and tokenizer from disk (or download first).
    ///
    /// # Errors
    ///
    /// Returns [`RerankerError::Permanent`] if the model files cannot be loaded
    /// or the ONNX session cannot be created.
    pub fn new(config: &RerankerConfig) -> Result<Self, RerankerError> {
        let paths = download::resolve_model_paths(config)?;

        info!("loading ONNX reranker model from {}", paths.onnx_path.display());

        #[cfg(feature = "reranker-cuda")]
        let session_builder = Session::builder()
            .map_err(|e| RerankerError::Permanent(Box::new(e)))?
            .with_execution_providers([CUDAExecutionProvider::default().build().error_on_failure()])
            .map_err(|e| RerankerError::Permanent(e.to_string().into()))?;
        #[cfg(not(feature = "reranker-cuda"))]
        let session_builder = Session::builder().map_err(|e| RerankerError::Permanent(Box::new(e)))?;

        let session = session_builder.commit_from_file(&paths.onnx_path).map_err(|e| RerankerError::Permanent(Box::new(e)))?;

        let mut tokenizer = tokenizers::Tokenizer::from_file(&paths.tokenizer_path).map_err(RerankerError::Permanent)?;

        // Ensure truncation is enabled so long documents don't exceed the
        // model's max position embeddings. Respect any limit already set in
        // tokenizer.json (custom models may have a smaller context window);
        // only apply the default (512) when no truncation is configured.
        if tokenizer.get_truncation().is_none() {
            let trunc_params = tokenizers::TruncationParams {
                max_length: Self::DEFAULT_MAX_LENGTH,
                ..Default::default()
            };
            #[expect(unused_results, reason = "with_truncation returns &mut Self for chaining — we don't need the reference")]
            tokenizer.with_truncation(Some(trunc_params)).map_err(RerankerError::Permanent)?;
        }

        // Detect which inputs the ONNX graph expects so we can skip
        // token_type_ids for models that don't declare it (e.g. RoBERTa).
        let input_names: std::collections::HashSet<String> = session.inputs.iter().map(|o| o.name.clone()).collect();
        let has_token_type_ids = input_names.contains("token_type_ids");

        let trunc_len = tokenizer.get_truncation().map_or(0, |t| t.max_length);
        info!(
            "ONNX reranker model loaded (inputs: [{}], truncation: {})",
            input_names.into_iter().collect::<Vec<_>>().join(", "),
            trunc_len
        );

        Ok(Self {
            inner: Arc::new(OnnxRerankerInner {
                session: Mutex::new(session),
                tokenizer,
                has_token_type_ids,
            }),
        })
    }

    /// Run cross-encoder inference on (query, document) pairs.
    ///
    /// This is the synchronous core called from the async `rerank` method.
    #[expect(clippy::arithmetic_side_effects, reason = "tensor index arithmetic is bounded by batch_size * max_len (checked above)")]
    #[expect(clippy::float_arithmetic, reason = "logit subtraction and sigmoid are core scoring arithmetic")]
    #[expect(clippy::integer_division, reason = "stride = logits.len() / batch_size is exact for well-formed model output")]
    #[expect(clippy::integer_division_remainder_used, reason = "stride computation uses integer division intentionally")]
    fn rerank_sync(inner: &OnnxRerankerInner, query: &str, documents: &[String]) -> Result<Vec<RerankerScore>, RerankerError> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }

        // Build (query, document) pairs for the cross-encoder
        let pairs: Vec<tokenizers::EncodeInput<'_>> = documents.iter().map(|doc| tokenizers::EncodeInput::Dual(query.into(), doc.as_str().into())).collect();

        // Tokenize all pairs
        let encodings = inner.tokenizer.encode_batch(pairs, true).map_err(RerankerError::Transient)?;

        let batch_size = encodings.len();

        // Find max sequence length for padding
        let max_len = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);

        if max_len == 0 {
            return Ok(Vec::new());
        }

        // Build flattened tensors with padding
        let total_elements = batch_size
            .checked_mul(max_len)
            .ok_or_else(|| RerankerError::Permanent("batch too large for tensor allocation".into()))?;
        let mut input_ids = vec![0_i64; total_elements];
        let mut attention_mask = vec![0_i64; total_elements];
        let mut token_type_ids = if inner.has_token_type_ids { vec![0_i64; total_elements] } else { Vec::new() };

        for (i, encoding) in encodings.iter().enumerate() {
            let offset = i * max_len;
            for (j, &id) in encoding.get_ids().iter().enumerate() {
                input_ids[offset + j] = i64::from(id);
            }
            for (j, &mask) in encoding.get_attention_mask().iter().enumerate() {
                attention_mask[offset + j] = i64::from(mask);
            }
            if inner.has_token_type_ids {
                fill_type_ids(&mut token_type_ids, encoding, offset);
            }
        }

        let shape = [batch_size, max_len];

        // Create ONNX tensors
        let input_ids_tensor = Tensor::from_array((shape, input_ids.into_boxed_slice())).map_err(|e| RerankerError::Transient(Box::new(e)))?;
        let attention_mask_tensor = Tensor::from_array((shape, attention_mask.into_boxed_slice())).map_err(|e| RerankerError::Transient(Box::new(e)))?;

        // Run inference and extract logits while the session lock is held.
        // SessionOutputs borrows from the Session, so we must copy the logits
        // into an owned Vec before releasing the MutexGuard.
        let logits: Vec<f32> = {
            let mut session = inner.session.lock();
            let outputs = if inner.has_token_type_ids {
                let token_type_ids_tensor = Tensor::from_array((shape, token_type_ids.into_boxed_slice())).map_err(|e| RerankerError::Transient(Box::new(e)))?;
                session.run(ort::inputs![
                    "input_ids" => input_ids_tensor,
                    "attention_mask" => attention_mask_tensor,
                    "token_type_ids" => token_type_ids_tensor,
                ])
            } else {
                session.run(ort::inputs![
                    "input_ids" => input_ids_tensor,
                    "attention_mask" => attention_mask_tensor,
                ])
            }
            .map_err(|e| RerankerError::Transient(Box::new(e)))?;

            let output_tensor = &outputs[0];
            let (_shape, raw_logits) = output_tensor.try_extract_tensor::<f32>().map_err(|e| RerankerError::Transient(Box::new(e)))?;
            raw_logits.to_vec()
        };

        // Build scores with sigmoid normalization.
        // Output shape is [batch_size] for regression models or [batch_size, C]
        // for classification models. We handle stride 1 (single logit per pair)
        // and stride 2 (two-class: use logit[1] - logit[0] as relevance signal).
        let stride = logits.len() / batch_size;
        let mut scores = Vec::with_capacity(batch_size);
        for i in 0..batch_size {
            let logit = match stride {
                0 => return Err(RerankerError::Permanent("ONNX model produced no logits".into())),
                1 => f64::from(logits[i]),
                2 => {
                    let base = i * 2;
                    f64::from(logits[base + 1]) - f64::from(logits[base])
                }
                other => {
                    return Err(RerankerError::Permanent(
                        format!("unsupported ONNX reranker output stride {other}: expected 1 or 2 logits per pair").into(),
                    ));
                }
            };
            let score = sigmoid(logit);
            scores.push(RerankerScore { index: i, score });
        }

        Ok(scores)
    }
}

/// Copy token type IDs from the encoding into the flattened tensor at the given offset.
#[expect(clippy::arithmetic_side_effects, reason = "tensor index arithmetic is bounded by batch_size * max_len (checked by caller)")]
fn fill_type_ids(token_type_ids: &mut [i64], encoding: &tokenizers::Encoding, offset: usize) {
    for (j, &tid) in encoding.get_type_ids().iter().enumerate() {
        token_type_ids[offset + j] = i64::from(tid);
    }
}

/// Sigmoid activation: maps logit to [0, 1].
#[expect(clippy::float_arithmetic, reason = "sigmoid is inherently a float arithmetic operation")]
fn sigmoid(x: f64) -> f64 {
    1.0_f64 / (1.0_f64 + (-x).exp())
}

impl RerankerProvider for OnnxReranker {
    fn rerank<'a>(&'a self, query: &'a str, documents: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<RerankerScore>, RerankerError>> {
        Box::pin(async move {
            let inner = Arc::clone(&self.inner);
            let query = query.to_owned();
            let documents: Vec<String> = documents.iter().map(|doc| (*doc).to_owned()).collect();
            tokio::task::spawn_blocking(move || Self::rerank_sync(&inner, &query, &documents))
                .await
                .map_err(|e| RerankerError::Transient(Box::new(e)))?
        })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), RerankerError>> {
        Box::pin(async move {
            let inner = Arc::clone(&self.inner);
            tokio::task::spawn_blocking(move || {
                let documents = vec!["test document".to_owned()];
                #[expect(let_underscore_drop, reason = "health check discards Vec<RerankerScore> immediately")]
                let _ = Self::rerank_sync(&inner, "test query", &documents)?;
                Ok::<_, RerankerError>(())
            })
            .await
            .map_err(|e| RerankerError::Transient(Box::new(e)))??;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::sigmoid;

    #[test]
    fn sigmoid_zero_is_half() {
        let result = sigmoid(0.0_f64);
        assert!((result - 0.5_f64).abs() < f64::EPSILON, "sigmoid(0) should be 0.5, got {result}");
    }

    #[test]
    fn sigmoid_large_positive_approaches_one() {
        let result = sigmoid(10.0_f64);
        assert!(result > 0.999_f64, "sigmoid(10) should be close to 1.0, got {result}");
    }

    #[test]
    fn sigmoid_large_negative_approaches_zero() {
        let result = sigmoid(-10.0_f64);
        assert!(result < 0.001_f64, "sigmoid(-10) should be close to 0.0, got {result}");
    }
}
