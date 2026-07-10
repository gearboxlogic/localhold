//! Streamable HTTP router construction for the MCP server.

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header::WWW_AUTHENTICATE},
    middleware::{self, Next},
    response::{IntoResponse as _, Response},
};
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager};
use tokio_util::sync::CancellationToken;
use tower_http::{limit::RequestBodyLimitLayer, trace::TraceLayer};

use crate::{
    config::{ServerConfig, validate_server_config},
    error::EngineError,
    http_auth::bearer_matches,
    server::RecallServer,
    store::MemoryStore,
};

/// Build the exact-path Streamable HTTP router used by the production binary.
///
/// When `server.http_auth_token` is configured, every request to the MCP
/// endpoint must present that bearer token. Requests to other paths remain
/// ordinary `404 Not Found` responses.
///
/// # Errors
///
/// Returns a configuration error if the configured endpoint path is invalid.
pub fn build_router<S>(server: RecallServer<S>, config: &ServerConfig, cancellation_token: &CancellationToken) -> Result<Router, EngineError>
where
    S: MemoryStore + Clone + std::fmt::Debug + 'static,
{
    validate_server_config(config)?;

    let mut http_config = StreamableHttpServerConfig::default();
    http_config.stateful_mode = true;
    http_config.sse_keep_alive = Some(Duration::from_secs(15));
    http_config.cancellation_token = cancellation_token.child_token();
    http_config.allowed_hosts.clone_from(&config.http_allowed_hosts);

    let service = StreamableHttpService::new(move || Ok(server.clone()), Arc::new(LocalSessionManager::default()), http_config);
    let mut router = Router::new().route_service(&config.path, service);

    if let Some(token) = config.http_auth_token.as_deref() {
        router = router.route_layer(middleware::from_fn_with_state(Arc::<str>::from(token), require_bearer));
    }

    Ok(router.layer(RequestBodyLimitLayer::new(config.max_body_bytes)).layer(TraceLayer::new_for_http()))
}

async fn require_bearer(State(expected): State<Arc<str>>, request: Request, next: Next) -> Response {
    if bearer_matches(request.headers(), &expected) {
        return next.run(request).await;
    }

    tracing::warn!(method = %request.method(), path = %request.uri().path(), "rejected unauthenticated HTTP MCP request");
    (StatusCode::UNAUTHORIZED, [(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))], "Unauthorized").into_response()
}
