use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use reqwest::{
    StatusCode,
    header::{AUTHORIZATION, HeaderMap, RETRY_AFTER},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{BoxFuture, EmbeddingProvider};
use crate::{
    clock::{Clock, SystemClock},
    config::{EmbeddingHealthCheck, OpenAiAuthMode, OpenAiCompatibleConfig},
    error::EmbeddingError,
};

const MAX_EMBEDDING_RESPONSE_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_MODELS_RESPONSE_BODY_BYTES: usize = 1024 * 1024;

/// Embedding provider backed by an OpenAI-compatible `/v1/embeddings` endpoint.
pub struct OpenAiEmbedding {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: Option<String>,
    auth_mode: OpenAiAuthMode,
    send_dimensions: bool,
    health_check: EmbeddingHealthCheck,
    dimensions: usize,
    timeout: Duration,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for OpenAiEmbedding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiEmbedding")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct EmbeddingRequest<'a, T> {
    model: &'a str,
    input: T,
    encoding_format: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<usize>,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelData>,
}

#[derive(Deserialize)]
struct ModelData {
    id: String,
}

/// L2-normalize a vector in place if it is not already unit-length.
///
/// # Errors
///
/// Returns `EmbeddingError::Permanent` if the vector is zero-length (norm == 0),
/// or contains `NaN` or `Inf` values.
#[expect(clippy::float_arithmetic, reason = "L2 normalization requires floating-point math")]
fn normalize_l2(embedding: &mut [f32]) -> Result<(), EmbeddingError> {
    let norm = embedding.iter().copied().map(|x| x * x).sum::<f32>().sqrt();
    if norm.is_nan() || norm.is_infinite() {
        return Err(EmbeddingError::Permanent("embedding contains NaN or Inf values".into()));
    }
    if norm == 0.0 {
        return Err(EmbeddingError::Permanent("cannot normalize zero vector".into()));
    }
    if (norm - 1.0).abs() > f32::EPSILON {
        for val in embedding {
            *val /= norm;
        }
    }
    Ok(())
}

fn classify_reqwest_error(err: reqwest::Error) -> EmbeddingError {
    if err.is_decode() {
        EmbeddingError::Permanent(Box::new(err))
    } else {
        EmbeddingError::Transient(Box::new(err))
    }
}

fn classify_http_status(status: StatusCode, context: &str, body: &str, retry_after: Option<Duration>) -> EmbeddingError {
    let message = format!("openai-compatible {context} failed with HTTP {status}: {body}");
    if status == StatusCode::TOO_MANY_REQUESTS {
        EmbeddingError::RateLimited {
            source: message.into(),
            retry_after,
        }
    } else if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT {
        EmbeddingError::Transient(message.into())
    } else {
        EmbeddingError::Permanent(message.into())
    }
}

fn parse_retry_after(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let retry_at = httpdate::parse_http_date(value).ok()?;
    Some(retry_at.duration_since(now).unwrap_or_default())
}

async fn read_error_body(mut response: reqwest::Response) -> String {
    const MAX_ERROR_BODY_BYTES: usize = 4_096;

    let mut body = Vec::with_capacity(MAX_ERROR_BODY_BYTES);
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(body.len());
                body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                if chunk.len() > remaining {
                    return format!("{}<truncated>", String::from_utf8_lossy(&body));
                }
            }
            Ok(None) => return String::from_utf8_lossy(&body).into_owned(),
            Err(error) => return format!("<failed to read response body: {error}>"),
        }
    }
}

fn oversized_success_body(context: &str, max_bytes: usize) -> EmbeddingError {
    EmbeddingError::Permanent(format!("openai-compatible {context} exceeds the {max_bytes}-byte response limit").into())
}

async fn read_success_json<T: DeserializeOwned>(mut response: reqwest::Response, max_bytes: usize, context: &str) -> Result<T, EmbeddingError> {
    let max_bytes_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if response.content_length().is_some_and(|length| length > max_bytes_u64) {
        return Err(oversized_success_body(context, max_bytes));
    }

    let initial_capacity = response.content_length().and_then(|length| usize::try_from(length).ok()).unwrap_or_default();
    let mut body = Vec::with_capacity(initial_capacity);
    while let Some(chunk) = response.chunk().await.map_err(classify_reqwest_error)? {
        let next_len = body.len().checked_add(chunk.len()).ok_or_else(|| oversized_success_body(context, max_bytes))?;
        if next_len > max_bytes {
            return Err(oversized_success_body(context, max_bytes));
        }
        body.extend_from_slice(&chunk);
    }

    serde_json::from_slice(&body).map_err(|error| EmbeddingError::Permanent(format!("failed to decode openai-compatible {context}: {error}").into()))
}

fn validate_and_normalize(embedding: &mut [f32], expected_dimensions: usize) -> Result<(), EmbeddingError> {
    if embedding.len() != expected_dimensions {
        return Err(EmbeddingError::Permanent(
            format!("expected {expected_dimensions} dimensions, got {}", embedding.len()).into(),
        ));
    }
    normalize_l2(embedding)
}

fn ordered_embeddings(response: EmbeddingResponse, expected_count: usize, expected_dimensions: usize) -> Result<Vec<Vec<f32>>, EmbeddingError> {
    if response.data.len() != expected_count {
        return Err(EmbeddingError::Permanent(
            format!("expected {expected_count} embeddings, got {}", response.data.len()).into(),
        ));
    }

    let mut embeddings: Vec<Option<Vec<f32>>> = std::iter::repeat_with(|| None).take(expected_count).collect();
    for item in response.data {
        if item.index >= expected_count {
            return Err(EmbeddingError::Permanent(
                format!("embedding response index {} is out of range for {expected_count} inputs", item.index).into(),
            ));
        }
        if embeddings[item.index].is_some() {
            return Err(EmbeddingError::Permanent(format!("duplicate embedding response index {}", item.index).into()));
        }

        let mut embedding = item.embedding;
        validate_and_normalize(&mut embedding, expected_dimensions)?;
        embeddings[item.index] = Some(embedding);
    }

    embeddings
        .into_iter()
        .enumerate()
        .map(|(index, embedding)| embedding.ok_or_else(|| EmbeddingError::Permanent(format!("missing embedding response index {index}").into())))
        .collect()
}

impl OpenAiEmbedding {
    /// Create a new OpenAI-compatible embedding provider from config.
    ///
    /// # Errors
    ///
    /// Returns a transient embedding error if the HTTP client cannot be built.
    pub fn new(config: &OpenAiCompatibleConfig, dimensions: usize, timeout: Duration) -> Result<Self, EmbeddingError> {
        Self::new_with_clock(config, dimensions, timeout, Arc::new(SystemClock::new()))
    }

    /// Create a provider with request deadlines driven by an injected clock.
    ///
    /// # Errors
    ///
    /// Returns a transient embedding error if the HTTP client cannot be built.
    pub fn new_with_clock(config: &OpenAiCompatibleConfig, dimensions: usize, timeout: Duration, clock: Arc<dyn Clock>) -> Result<Self, EmbeddingError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(classify_reqwest_error)?;
        Ok(Self {
            client,
            base_url: config.base_url.trim().trim_end_matches('/').to_owned(),
            model: config.model.clone(),
            api_key: config.api_key.clone(),
            auth_mode: config.auth_mode,
            send_dimensions: config.send_dimensions,
            health_check: config.health_check,
            dimensions,
            timeout,
            clock,
        })
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let request = self.client.request(method, self.endpoint(path));
        if let Some(api_key) = self.api_key.as_deref().filter(|api_key| !api_key.is_empty()) {
            match self.auth_mode {
                OpenAiAuthMode::Bearer => request.header(AUTHORIZATION, format!("Bearer {api_key}")),
                OpenAiAuthMode::ApiKey => request.header("api-key", api_key),
            }
        } else {
            request
        }
    }

    async fn post_embeddings<T: Serialize>(&self, input: T, expected_count: usize, context: &str) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let request = EmbeddingRequest {
            model: &self.model,
            input,
            encoding_format: "float",
            dimensions: self.send_dimensions.then_some(self.dimensions),
        };
        let response = self
            .request(reqwest::Method::POST, "embeddings")
            .json(&request)
            .send()
            .await
            .map_err(classify_reqwest_error)?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(response.headers(), self.clock.now().into());
            let body = read_error_body(response).await;
            return Err(classify_http_status(status, context, &body, retry_after));
        }

        let response = read_success_json::<EmbeddingResponse>(response, MAX_EMBEDDING_RESPONSE_BODY_BYTES, "embedding response").await?;
        ordered_embeddings(response, expected_count, self.dimensions)
    }

    async fn embed_impl(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        if text.trim().is_empty() {
            return Err(EmbeddingError::Permanent("cannot embed empty text".into()));
        }

        let embeddings = self.post_embeddings(text, 1, "embedding request").await?;
        embeddings.into_iter().next().ok_or_else(|| EmbeddingError::Permanent("empty embedding response".into()))
    }

    async fn embed_batch_impl(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if texts.len() == 1 {
            return self.embed_impl(texts[0]).await.map(|embedding| vec![embedding]);
        }
        if texts.iter().any(|text| text.trim().is_empty()) {
            return Err(EmbeddingError::Permanent("cannot embed empty text".into()));
        }

        self.post_embeddings(texts, texts.len(), "batch embedding request").await
    }

    async fn health_check_impl(&self) -> Result<(), EmbeddingError> {
        if self.health_check == EmbeddingHealthCheck::Disabled {
            return Ok(());
        }

        let response = self.request(reqwest::Method::GET, "models").send().await.map_err(classify_reqwest_error)?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(response.headers(), self.clock.now().into());
            let body = read_error_body(response).await;
            return Err(classify_http_status(status, "health check", &body, retry_after));
        }

        let models = read_success_json::<ModelsResponse>(response, MAX_MODELS_RESPONSE_BODY_BYTES, "model-list response").await?;
        let model_found = models.data.iter().any(|model| model.id == self.model || model.id.starts_with(&format!("{}:", self.model)));
        if !model_found {
            return Err(EmbeddingError::Permanent(
                format!("configured model {:?} not found in OpenAI-compatible model list", self.model).into(),
            ));
        }

        Ok(())
    }
}

impl EmbeddingProvider for OpenAiEmbedding {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        Box::pin(async move {
            crate::clock::timeout(self.clock.as_ref(), self.timeout, self.embed_impl(text))
                .await
                .map_err(|_elapsed| EmbeddingError::Transient("embedding request timed out".into()))?
        })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
        Box::pin(async move {
            crate::clock::timeout(self.clock.as_ref(), self.timeout, self.health_check_impl())
                .await
                .map_err(|_elapsed| EmbeddingError::Transient("embedding health check timed out".into()))?
        })
    }

    fn embed_batch<'a>(&'a self, texts: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
        Box::pin(async move {
            crate::clock::timeout(self.clock.as_ref(), self.timeout, self.embed_batch_impl(texts))
                .await
                .map_err(|_elapsed| EmbeddingError::Transient("embedding batch request timed out".into()))?
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Router,
        body::{Body, Bytes, to_bytes},
        extract::{Request, State},
        http::{
            StatusCode,
            header::{AUTHORIZATION as AXUM_AUTHORIZATION, CONTENT_LENGTH},
        },
        response::{Redirect, Response},
        routing::{get, post},
    };
    use serde_json::{Value, json};
    use tokio::task::JoinHandle;

    use super::{MAX_EMBEDDING_RESPONSE_BODY_BYTES, MAX_MODELS_RESPONSE_BODY_BYTES, OpenAiEmbedding, read_success_json};
    use crate::{
        clock::MockClock,
        config::{EmbeddingHealthCheck, OpenAiAuthMode, OpenAiCompatibleConfig},
        embedding::EmbeddingProvider as _,
        error::EmbeddingError,
    };

    #[derive(Clone)]
    struct MockState {
        embed_status: StatusCode,
        embed_body: Arc<str>,
        expected_auth: Option<Arc<str>>,
        expected_api_key: Option<Arc<str>>,
        expected_dimensions: Option<u64>,
        models_status: StatusCode,
        models_body: Arc<str>,
    }

    #[derive(Clone, Copy)]
    struct ProviderSetup<'a> {
        embed_status: StatusCode,
        embed_body: &'a str,
        models_status: StatusCode,
        models_body: &'a str,
        dimensions: usize,
        api_key: Option<&'a str>,
        auth_mode: OpenAiAuthMode,
        send_dimensions: bool,
        health_check: EmbeddingHealthCheck,
    }

    async fn embeddings_handler(State(state): State<MockState>, request: Request) -> (StatusCode, [(&'static str, &'static str); 1], String) {
        let authorization = request.headers().get(AXUM_AUTHORIZATION).and_then(|value| value.to_str().ok()).map(ToOwned::to_owned);
        let api_key = request.headers().get("api-key").and_then(|value| value.to_str().ok()).map(ToOwned::to_owned);
        if authorization.as_deref() != state.expected_auth.as_deref() {
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                format!(r#"{{"error":"unexpected authorization header: {authorization:?}"}}"#),
            );
        }
        if api_key.as_deref() != state.expected_api_key.as_deref() {
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                format!(r#"{{"error":"unexpected api-key header: {api_key:?}"}}"#),
            );
        }
        let body = to_bytes(request.into_body(), 1_048_576).await.unwrap();
        let request: Value = serde_json::from_slice(&body).unwrap();
        if request.get("dimensions").and_then(Value::as_u64) != state.expected_dimensions {
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                format!(r#"{{"error":"unexpected dimensions field: {:?}"}}"#, request.get("dimensions")),
            );
        }
        let body = if state.embed_body.as_ref() == "__echo_request__" {
            json!({ "request": request }).to_string()
        } else {
            (*state.embed_body).to_owned()
        };
        (state.embed_status, [("content-type", "application/json")], body)
    }

    async fn models_handler(State(state): State<MockState>) -> (StatusCode, [(&'static str, &'static str); 1], String) {
        (state.models_status, [("content-type", "application/json")], (*state.models_body).to_owned())
    }

    async fn redirected_embeddings_handler(State(counter): State<Arc<AtomicUsize>>) -> (StatusCode, &'static str) {
        let _previous = counter.fetch_add(1, Ordering::Relaxed);
        (StatusCode::OK, r#"{"data":[{"index":0,"embedding":[1.0,0.0,0.0]}]}"#)
    }

    async fn rate_limited_embeddings_handler() -> (StatusCode, [(&'static str, &'static str); 1], &'static str) {
        (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "4")], "quota exceeded")
    }

    async fn oversized_error_handler() -> (StatusCode, String) {
        (StatusCode::BAD_REQUEST, "x".repeat(5_000))
    }

    async fn oversized_embedding_success_handler() -> Response {
        let body = Body::from_stream(futures::stream::pending::<Result<Bytes, Infallible>>());
        Response::builder()
            .header(CONTENT_LENGTH, MAX_EMBEDDING_RESPONSE_BODY_BYTES.saturating_add(1))
            .body(body)
            .unwrap()
    }

    async fn oversized_models_success_handler() -> Response {
        let body = Body::from_stream(futures::stream::pending::<Result<Bytes, Infallible>>());
        Response::builder()
            .header(CONTENT_LENGTH, MAX_MODELS_RESPONSE_BODY_BYTES.saturating_add(1))
            .body(body)
            .unwrap()
    }

    async fn oversized_chunked_success_handler() -> Response {
        let chunks = futures::stream::iter([Ok::<_, Infallible>(Bytes::from(vec![b' '; 32])), Ok::<_, Infallible>(Bytes::from_static(b"x"))]);
        Response::new(Body::from_stream(chunks))
    }

    async fn hanging_embeddings_handler() -> (StatusCode, &'static str) {
        std::future::pending().await
    }

    async fn batch_contract_handler(request: Request) -> (StatusCode, [(&'static str, &'static str); 1], String) {
        let body = to_bytes(request.into_body(), 1_048_576).await.unwrap();
        let request: Value = serde_json::from_slice(&body).unwrap();
        let valid = request.get("model") == Some(&json!("test-model"))
            && request.get("input") == Some(&json!(["first", "second"]))
            && request.get("encoding_format") == Some(&json!("float"))
            && request.get("dimensions").is_none();
        if !valid {
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                json!({"error": "unexpected batch request", "request": request}).to_string(),
            );
        }
        (
            StatusCode::OK,
            [("content-type", "application/json")],
            r#"{"data":[{"index":1,"embedding":[0.0,5.0,0.0]},{"index":0,"embedding":[3.0,4.0,0.0]}]}"#.to_owned(),
        )
    }

    async fn provider_for_router(app: Router) -> (OpenAiEmbedding, JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let _result = axum::serve(listener, app).await;
        });
        let config = OpenAiCompatibleConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "test-model".into(),
            ..OpenAiCompatibleConfig::default()
        };
        (OpenAiEmbedding::new(&config, 3, Duration::from_secs(30)).unwrap(), server)
    }

    fn build_router(state: MockState) -> Router {
        Router::new()
            .route("/v1/embeddings", post(embeddings_handler))
            .route("/v1/models", get(models_handler))
            .with_state(state)
    }

    async fn setup_provider(embed_body: &str, dimensions: usize) -> (OpenAiEmbedding, JoinHandle<()>) {
        setup_provider_with_status(StatusCode::OK, embed_body, StatusCode::OK, r#"{"data":[{"id":"test-model:latest"}]}"#, dimensions).await
    }

    async fn setup_provider_with_status(
        embed_status: StatusCode,
        embed_body: &str,
        models_status: StatusCode,
        models_body: &str,
        dimensions: usize,
    ) -> (OpenAiEmbedding, JoinHandle<()>) {
        setup_provider_with(ProviderSetup {
            embed_status,
            embed_body,
            models_status,
            models_body,
            dimensions,
            api_key: Some("test-key"),
            auth_mode: OpenAiAuthMode::Bearer,
            send_dimensions: false,
            health_check: EmbeddingHealthCheck::Models,
        })
        .await
    }

    async fn setup_provider_with(setup: ProviderSetup<'_>) -> (OpenAiEmbedding, JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = build_router(MockState {
            embed_status: setup.embed_status,
            embed_body: Arc::from(setup.embed_body),
            expected_auth: (setup.auth_mode == OpenAiAuthMode::Bearer)
                .then(|| setup.api_key.map(|key| Arc::from(format!("Bearer {key}"))))
                .flatten(),
            expected_api_key: (setup.auth_mode == OpenAiAuthMode::ApiKey).then(|| setup.api_key.map(Arc::from)).flatten(),
            expected_dimensions: setup.send_dimensions.then_some(setup.dimensions).and_then(|dimensions| u64::try_from(dimensions).ok()),
            models_status: setup.models_status,
            models_body: Arc::from(setup.models_body),
        });

        let server = tokio::spawn(async move {
            #[expect(clippy::let_underscore_must_use, reason = "fire-and-forget mock server for test")]
            #[expect(let_underscore_drop, reason = "Result dropped immediately is fine — server runs in spawned task")]
            let _ = axum::serve(listener, app).await;
        });

        let config = OpenAiCompatibleConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "test-model".into(),
            api_key: setup.api_key.map(ToOwned::to_owned),
            auth_mode: setup.auth_mode,
            send_dimensions: setup.send_dimensions,
            health_check: setup.health_check,
            ..OpenAiCompatibleConfig::default()
        };
        (OpenAiEmbedding::new(&config, setup.dimensions, Duration::from_secs(30)).unwrap(), server)
    }

    #[tokio::test]
    async fn embeds_single_text() {
        let (provider, server) = setup_provider(r#"{"data":[{"index":0,"embedding":[3.0,4.0,0.0]}]}"#, 3).await;
        let embedding = provider.embed("hello").await.unwrap();
        assert_eq!(embedding, vec![0.6, 0.8, 0.0]);
        server.abort();
    }

    #[tokio::test]
    async fn request_timeout_is_driven_by_injected_clock() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let _result = axum::serve(listener, Router::new().route("/v1/embeddings", post(hanging_embeddings_handler))).await;
        });
        let config = OpenAiCompatibleConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "test-model".into(),
            ..OpenAiCompatibleConfig::default()
        };
        let clock = Arc::new(MockClock::new());
        let provider = OpenAiEmbedding::new_with_clock(&config, 3, Duration::from_secs(30), Arc::<MockClock>::clone(&clock)).unwrap();
        let request = provider.embed("hello");
        tokio::pin!(request);
        assert!(futures::poll!(request.as_mut()).is_pending());

        clock.advance(chrono::TimeDelta::seconds(30));
        let error = request.await.unwrap_err();
        assert!(matches!(error, EmbeddingError::Transient(_)));
        assert!(error.to_string().contains("timed out"));
        server.abort();
    }

    #[tokio::test]
    async fn embeds_batch_and_restores_response_order() {
        let (provider, server) = setup_provider(r#"{"data":[{"index":1,"embedding":[0.0,5.0,0.0]},{"index":0,"embedding":[3.0,4.0,0.0]}]}"#, 3).await;
        let embeddings = provider.embed_batch(&["first", "second"]).await.unwrap();
        assert_eq!(embeddings, vec![vec![0.6, 0.8, 0.0], vec![0.0, 1.0, 0.0]]);
        server.abort();
    }

    #[tokio::test]
    async fn batch_request_uses_openai_array_input_contract() {
        let app = Router::new().route("/v1/embeddings", post(batch_contract_handler));
        let (provider, server) = provider_for_router(app).await;

        let embeddings = provider.embed_batch(&["first", "second"]).await.unwrap();
        assert_eq!(embeddings, vec![vec![0.6, 0.8, 0.0], vec![0.0, 1.0, 0.0]]);
        server.abort();
    }

    #[tokio::test]
    async fn omitted_api_key_sends_no_authorization_header() {
        let (provider, server) = setup_provider_with(ProviderSetup {
            embed_status: StatusCode::OK,
            embed_body: r#"{"data":[{"index":0,"embedding":[3.0,4.0,0.0]}]}"#,
            models_status: StatusCode::OK,
            models_body: r#"{"data":[{"id":"test-model:latest"}]}"#,
            dimensions: 3,
            api_key: None,
            auth_mode: OpenAiAuthMode::Bearer,
            send_dimensions: false,
            health_check: EmbeddingHealthCheck::Models,
        })
        .await;
        let embedding = provider.embed("hello").await.unwrap();
        assert_eq!(embedding, vec![0.6, 0.8, 0.0]);
        server.abort();
    }

    #[tokio::test]
    async fn api_key_auth_mode_uses_azure_header() {
        let (provider, server) = setup_provider_with(ProviderSetup {
            embed_status: StatusCode::OK,
            embed_body: r#"{"data":[{"index":0,"embedding":[3.0,4.0,0.0]}]}"#,
            models_status: StatusCode::OK,
            models_body: r#"{"data":[{"id":"test-model"}]}"#,
            dimensions: 3,
            api_key: Some("azure-key"),
            auth_mode: OpenAiAuthMode::ApiKey,
            send_dimensions: false,
            health_check: EmbeddingHealthCheck::Models,
        })
        .await;
        let _embedding = provider.embed("hello").await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn configured_dimensions_are_included_in_request() {
        let (provider, server) = setup_provider_with(ProviderSetup {
            embed_status: StatusCode::OK,
            embed_body: r#"{"data":[{"index":0,"embedding":[3.0,4.0,0.0]}]}"#,
            models_status: StatusCode::OK,
            models_body: r#"{"data":[{"id":"test-model"}]}"#,
            dimensions: 3,
            api_key: Some("test-key"),
            auth_mode: OpenAiAuthMode::Bearer,
            send_dimensions: true,
            health_check: EmbeddingHealthCheck::Models,
        })
        .await;
        let _embedding = provider.embed("hello").await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn disabled_health_check_does_not_require_models_endpoint() {
        let (provider, server) = setup_provider_with(ProviderSetup {
            embed_status: StatusCode::OK,
            embed_body: r#"{"data":[{"index":0,"embedding":[3.0,4.0,0.0]}]}"#,
            models_status: StatusCode::NOT_FOUND,
            models_body: "models endpoint unavailable",
            dimensions: 3,
            api_key: Some("test-key"),
            auth_mode: OpenAiAuthMode::Bearer,
            send_dimensions: false,
            health_check: EmbeddingHealthCheck::Disabled,
        })
        .await;
        provider.health_check().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn rejects_dimension_mismatch() {
        let (provider, server) = setup_provider(r#"{"data":[{"index":0,"embedding":[0.1,0.2]}]}"#, 3).await;
        let err = provider.embed("hello").await.unwrap_err();
        assert!(matches!(err, EmbeddingError::Permanent(_)));
        assert!(err.to_string().contains("expected 3 dimensions"));
        server.abort();
    }

    #[tokio::test]
    async fn health_check_requires_configured_model() {
        let (provider, server) = setup_provider_with_status(StatusCode::OK, r#"{"data":[]}"#, StatusCode::OK, r#"{"data":[{"id":"other-model"}]}"#, 3).await;
        let err = provider.health_check().await.unwrap_err();
        assert!(matches!(err, EmbeddingError::Permanent(_)));
        assert!(err.to_string().contains("not found"));
        server.abort();
    }

    #[tokio::test]
    async fn http_5xx_is_transient() {
        let (provider, server) = setup_provider_with_status(StatusCode::SERVICE_UNAVAILABLE, "upstream down", StatusCode::OK, r#"{"data":[{"id":"test-model"}]}"#, 3).await;
        let err = provider.embed("hello").await.unwrap_err();
        assert!(matches!(err, EmbeddingError::Transient(_)));
        server.abort();
    }

    #[tokio::test]
    async fn http_429_preserves_retry_after() {
        let app = Router::new().route("/v1/embeddings", post(rate_limited_embeddings_handler));
        let (provider, server) = provider_for_router(app).await;

        let error = provider.embed("hello").await.unwrap_err();
        assert!(matches!(
            error,
            EmbeddingError::RateLimited {
                retry_after: Some(delay),
                ..
            } if delay == Duration::from_secs(4)
        ));
        server.abort();
    }

    #[test]
    fn retry_after_accepts_http_dates() {
        let mut headers = reqwest::header::HeaderMap::new();
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let retry_at = now + Duration::from_secs(10);
        let _previous = headers.insert(reqwest::header::RETRY_AFTER, httpdate::fmt_http_date(retry_at).parse().unwrap());

        let delay = super::parse_retry_after(&headers, now).unwrap();
        assert_eq!(delay, Duration::from_secs(10));
    }

    #[tokio::test]
    async fn oversized_error_body_is_truncated() {
        let app = Router::new().route("/v1/embeddings", post(oversized_error_handler));
        let (provider, server) = provider_for_router(app).await;

        let error = provider.embed("hello").await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("<truncated>"));
        assert!(message.len() < 4_300);
        server.abort();
    }

    #[tokio::test]
    async fn oversized_embedding_success_body_is_rejected_from_content_length() {
        let app = Router::new().route("/v1/embeddings", post(oversized_embedding_success_handler));
        let (provider, server) = provider_for_router(app).await;

        let error = provider.embed("hello").await.unwrap_err();
        assert!(matches!(error, EmbeddingError::Permanent(_)));
        let message = error.to_string();
        assert!(message.contains("embedding response"), "unexpected error: {message}");
        assert!(message.contains("response limit"), "unexpected error: {message}");
        server.abort();
    }

    #[tokio::test]
    async fn oversized_models_success_body_is_rejected_from_content_length() {
        let app = Router::new().route("/v1/models", get(oversized_models_success_handler));
        let (provider, server) = provider_for_router(app).await;

        let error = provider.health_check().await.unwrap_err();
        assert!(matches!(error, EmbeddingError::Permanent(_)));
        let message = error.to_string();
        assert!(message.contains("model-list response"), "unexpected error: {message}");
        assert!(message.contains("response limit"), "unexpected error: {message}");
        server.abort();
    }

    #[tokio::test]
    async fn chunked_success_body_is_bounded_while_streaming() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let _result = axum::serve(listener, Router::new().route("/body", get(oversized_chunked_success_handler))).await;
        });
        let response = reqwest::get(format!("http://127.0.0.1:{port}/body")).await.unwrap();
        assert_eq!(response.content_length(), None);

        let error = read_success_json::<Value>(response, 32, "test response").await.unwrap_err();
        assert!(matches!(error, EmbeddingError::Permanent(_)));
        assert!(error.to_string().contains("32-byte response limit"));
        server.abort();
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let redirected_requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&redirected_requests);
        let app = Router::new()
            .route("/v1/embeddings", post(|| async { Redirect::temporary("/redirected") }))
            .route("/redirected", post(redirected_embeddings_handler))
            .with_state(counter);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let _result = axum::serve(listener, app).await;
        });
        let config = OpenAiCompatibleConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "test-model".into(),
            api_key: Some("must-not-be-forwarded".into()),
            ..OpenAiCompatibleConfig::default()
        };
        let provider = OpenAiEmbedding::new(&config, 3, Duration::from_secs(30)).unwrap();

        let error = provider.embed("hello").await.unwrap_err();
        assert!(matches!(error, EmbeddingError::Permanent(_)));
        assert_eq!(redirected_requests.load(Ordering::Relaxed), 0);
        server.abort();
    }

    #[tokio::test]
    async fn malformed_success_json_is_permanent() {
        let (provider, server) = setup_provider("not-json", 3).await;
        let err = provider.embed("hello").await.unwrap_err();
        assert!(matches!(err, EmbeddingError::Permanent(_)));
        server.abort();
    }
}
