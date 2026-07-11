//! Live smoke tests for the real ONNX reranker.

#![cfg(feature = "reranker")]

use localhold::{
    config::{RerankerConfig, RerankerExecutionProvider},
    reranker::runtime::initialize_with_retry,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "loads the real ONNX reranker model; intended for local smoke testing"]
async fn live_onnx_reranker_scores_documents() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = RerankerConfig::default();
    config.enabled = true;
    let requested_provider = std::env::var("LOCALHOLD_TEST_RERANKER_EXECUTION_PROVIDER").unwrap_or_else(|_| "cpu".into()).parse()?;
    config.execution_provider = requested_provider;
    let reranker = initialize_with_retry(&config).await?;

    let selected = reranker.selected_execution_provider().ok_or("reranker initialized without a selected execution provider")?;
    let expected_selected = std::env::var("LOCALHOLD_TEST_RERANKER_EXPECTED_PROVIDER")
        .ok()
        .map(|value| value.parse::<RerankerExecutionProvider>())
        .transpose()?;
    let provider_matches = match expected_selected {
        Some(expected) => selected == expected,
        None => match requested_provider {
            RerankerExecutionProvider::Auto => matches!(selected, RerankerExecutionProvider::Cpu | RerankerExecutionProvider::Cuda),
            RerankerExecutionProvider::Cpu => selected == RerankerExecutionProvider::Cpu,
            RerankerExecutionProvider::Cuda => selected == RerankerExecutionProvider::Cuda,
            _ => return Err("unsupported reranker execution provider in smoke test".into()),
        },
    };
    if !provider_matches {
        return Err(format!("requested {requested_provider}, expected {expected_selected:?}, but selected {selected}").into());
    }

    let provider = reranker.into_provider();
    provider.health_check().await?;

    let documents = [
        "Rust ownership and borrow checker rules for references",
        "Sourdough starter feeding schedule and bread hydration",
    ];
    let scores = provider.rerank("rust borrow checker ownership", &documents).await?;

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

    let iterations = std::env::var("LOCALHOLD_TEST_RERANKER_ITERATIONS").map_or(Ok(1_usize), |value| value.parse::<usize>())?;
    if iterations == 0 {
        return Err("LOCALHOLD_TEST_RERANKER_ITERATIONS must be greater than zero".into());
    }
    for _ in 1..iterations {
        let _scores = provider.rerank("rust borrow checker ownership", &documents).await?;
    }

    Ok(())
}
