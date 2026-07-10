//! Live smoke tests for the real ONNX reranker.

#![cfg(feature = "reranker")]

use localhold::{
    config::RerankerConfig,
    reranker::{RerankerProvider as _, onnx::OnnxReranker},
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "loads the real ONNX reranker model; intended for local smoke testing"]
async fn live_onnx_reranker_scores_documents() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = RerankerConfig::default();
    config.enabled = true;
    let reranker = OnnxReranker::new(&config)?;

    reranker.health_check().await?;

    let documents = [
        "Rust ownership and borrow checker rules for references",
        "Sourdough starter feeding schedule and bread hydration",
    ];
    let scores = reranker.rerank("rust borrow checker ownership", &documents).await?;

    if scores.len() != documents.len() {
        return Err(format!("expected {} reranker scores, got {}", documents.len(), scores.len()).into());
    }
    for score in &scores {
        if !score.score.is_finite() {
            return Err(format!("reranker score should be finite: {score:?}").into());
        }
        if !(0.0_f64..=1.0_f64).contains(&score.score) {
            return Err(format!("reranker score should be normalized: {score:?}").into());
        }
    }

    Ok(())
}
