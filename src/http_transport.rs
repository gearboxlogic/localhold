//! Streamable HTTP router construction for the MCP server.

use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header::WWW_AUTHENTICATE},
    middleware::{self, Next},
    response::{IntoResponse as _, Response},
};
use futures::Stream;
use parking_lot::Mutex;
use rmcp::{
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::streamable_http_server::{
        RestoreOutcome, SessionId, SessionManager, StreamableHttpServerConfig, StreamableHttpService,
        session::{
            ServerSseMessage,
            local::{LocalSessionManager, LocalSessionManagerError},
        },
    },
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tower_http::{limit::RequestBodyLimitLayer, trace::TraceLayer};

use crate::{
    clock::{Clock, SystemClock},
    config::{ServerConfig, validate_server_config},
    error::EngineError,
    http_auth::bearer_matches,
    server::LocalHoldServer,
    store::MemoryStore,
};

type SessionActivityMap = Arc<Mutex<HashMap<SessionId, SessionActivity>>>;

/// Build the exact-path Streamable HTTP router used by the production binary.
///
/// When `server.http_auth_token` is configured, every request to the MCP
/// endpoint must present that bearer token. Requests to other paths remain
/// ordinary `404 Not Found` responses.
///
/// # Errors
///
/// Returns a configuration error if the configured endpoint path is invalid.
pub fn build_router<S>(server: LocalHoldServer<S>, config: &ServerConfig, cancellation_token: &CancellationToken) -> Result<Router, EngineError>
where
    S: MemoryStore + Clone + std::fmt::Debug + 'static,
{
    build_router_with_clock(server, config, cancellation_token, Arc::new(SystemClock::new()))
}

/// Build the HTTP router with session expiry driven by an injected clock.
///
/// # Errors
///
/// Returns a configuration error if the configured endpoint path is invalid.
pub fn build_router_with_clock<S>(server: LocalHoldServer<S>, config: &ServerConfig, cancellation_token: &CancellationToken, clock: Arc<dyn Clock>) -> Result<Router, EngineError>
where
    S: MemoryStore + Clone + std::fmt::Debug + 'static,
{
    validate_server_config(config)?;
    let server = if config.admin_tools_enabled {
        server.with_admin_tools()
    } else {
        server.without_admin_tools()
    };

    let mut http_config = StreamableHttpServerConfig::default();
    http_config.stateful_mode = true;
    http_config.sse_keep_alive = Some(Duration::from_secs(15));
    http_config.cancellation_token = cancellation_token.child_token();
    http_config.allowed_hosts.clone_from(&config.http_allowed_hosts);

    let sessions = Arc::new(CappedSessionManager::new_with_clock(
        config.http_max_sessions,
        Duration::from_secs(config.http_session_idle_timeout_secs),
        clock,
    ));
    sessions.spawn_reaper(cancellation_token.child_token());
    let service = StreamableHttpService::new(move || Ok(server.clone()), sessions, http_config);
    let mut router = Router::new().route_service(&config.path, service);

    if let Some(token) = config.http_auth_token.as_deref() {
        router = router.route_layer(middleware::from_fn_with_state(Arc::<str>::from(token), require_bearer));
    }

    Ok(router.layer(RequestBodyLimitLayer::new(config.max_body_bytes)).layer(TraceLayer::new_for_http()))
}

#[derive(Debug)]
struct CappedSessionManager {
    inner: LocalSessionManager,
    max_sessions: usize,
    idle_timeout: Duration,
    activity: SessionActivityMap,
    create_lock: tokio::sync::Mutex<()>,
    clock: Arc<dyn Clock>,
}

impl CappedSessionManager {
    #[cfg(test)]
    fn new(max_sessions: usize, idle_timeout: Duration) -> Self {
        Self::new_with_clock(max_sessions, idle_timeout, Arc::new(SystemClock::new()))
    }

    fn new_with_clock(max_sessions: usize, idle_timeout: Duration, clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: LocalSessionManager::default(),
            max_sessions,
            idle_timeout,
            activity: Arc::new(Mutex::new(HashMap::new())),
            create_lock: tokio::sync::Mutex::new(()),
            clock,
        }
    }

    async fn ensure_capacity(&self) -> Result<(), CappedSessionManagerError> {
        if self.inner.sessions.read().await.len() >= self.max_sessions {
            return Err(CappedSessionManagerError::Capacity(self.max_sessions));
        }
        Ok(())
    }

    fn touch(&self, id: &SessionId) {
        if let Some(activity) = self.activity.lock().get_mut(id) {
            activity.last_seen = self.clock.monotonic();
        }
    }

    fn track_stream<S>(&self, id: &SessionId, stream: S) -> ActivityStream<S>
    where
        S: Stream<Item = ServerSseMessage> + Send + Sync + 'static,
    {
        let mut sessions = self.activity.lock();
        let now = self.clock.monotonic();
        let activity = sessions.entry(Arc::clone(id)).or_insert_with(|| SessionActivity::at(now));
        activity.last_seen = now;
        activity.active_streams = activity.active_streams.saturating_add(1);
        drop(sessions);
        ActivityStream {
            inner: Box::pin(stream),
            id: Arc::clone(id),
            activity: Arc::clone(&self.activity),
            clock: Arc::clone(&self.clock),
        }
    }

    async fn reap_stale(&self) {
        let now = self.clock.monotonic();
        let stale = {
            let sessions = self.activity.lock();
            sessions
                .iter()
                .filter(|(_, activity)| activity.active_streams == 0 && now.saturating_sub(activity.last_seen) >= self.idle_timeout)
                .map(|(id, _)| Arc::clone(id))
                .collect::<Vec<_>>()
        };
        for id in stale {
            let now = self.clock.monotonic();
            let still_stale = self
                .activity
                .lock()
                .get(&id)
                .is_some_and(|activity| activity.active_streams == 0 && now.saturating_sub(activity.last_seen) >= self.idle_timeout);
            if !still_stale {
                continue;
            }
            if let Err(error) = self.inner.close_session(&id).await {
                tracing::warn!(session_id = %id, %error, "failed to reap idle HTTP MCP session");
            }
            let _removed = self.activity.lock().remove(&id);
        }
    }

    #[expect(clippy::integer_division_remainder_used, reason = "false positive from tokio::select macro expansion")]
    fn spawn_reaper(self: &Arc<Self>, cancellation_token: CancellationToken) {
        let manager = Arc::clone(self);
        let clock = Arc::clone(&self.clock);
        let interval = self.idle_timeout.min(Duration::from_secs(30)).max(Duration::from_secs(1));
        let _reaper = tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancellation_token.cancelled() => return,
                    () = clock.sleep(interval) => manager.reap_stale().await,
                }
            }
        });
    }
}

#[derive(Debug)]
struct SessionActivity {
    last_seen: Duration,
    active_streams: usize,
}

impl SessionActivity {
    const fn at(now: Duration) -> Self {
        Self {
            last_seen: now,
            active_streams: 0,
        }
    }
}

struct ActivityStream<S> {
    inner: Pin<Box<S>>,
    id: SessionId,
    activity: SessionActivityMap,
    clock: Arc<dyn Clock>,
}

impl<S: Stream<Item = ServerSseMessage>> Stream for ActivityStream<S> {
    type Item = ServerSseMessage;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let result = this.inner.as_mut().poll_next(cx);
        if matches!(result, Poll::Ready(Some(_)))
            && let Some(activity) = this.activity.lock().get_mut(&this.id)
        {
            activity.last_seen = this.clock.monotonic();
        }
        result
    }
}

impl<S> Drop for ActivityStream<S> {
    fn drop(&mut self) {
        if let Some(activity) = self.activity.lock().get_mut(&self.id) {
            activity.active_streams = activity.active_streams.saturating_sub(1);
            activity.last_seen = self.clock.monotonic();
        }
    }
}

#[derive(Debug, Error)]
enum CappedSessionManagerError {
    #[error("HTTP MCP session limit reached ({0})")]
    Capacity(usize),
    #[error(transparent)]
    Inner(#[from] LocalSessionManagerError),
}

impl SessionManager for CappedSessionManager {
    type Error = CappedSessionManagerError;
    type Transport = <LocalSessionManager as SessionManager>::Transport;

    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        let _guard = self.create_lock.lock().await;
        self.ensure_capacity().await?;
        let (id, transport) = self.inner.create_session().await?;
        let _previous = self.activity.lock().insert(Arc::clone(&id), SessionActivity::at(self.clock.monotonic()));
        Ok((id, transport))
    }

    async fn initialize_session(&self, id: &SessionId, message: ClientJsonRpcMessage) -> Result<ServerJsonRpcMessage, Self::Error> {
        let response = self.inner.initialize_session(id, message).await?;
        self.touch(id);
        Ok(response)
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        let exists = self.inner.has_session(id).await?;
        if exists {
            self.touch(id);
        }
        Ok(exists)
    }

    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        self.inner.close_session(id).await?;
        let _removed = self.activity.lock().remove(id);
        Ok(())
    }

    async fn create_stream(&self, id: &SessionId, message: ClientJsonRpcMessage) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        let stream = self.inner.create_stream(id, message).await?;
        Ok(self.track_stream(id, stream))
    }

    async fn accept_message(&self, id: &SessionId, message: ClientJsonRpcMessage) -> Result<(), Self::Error> {
        self.inner.accept_message(id, message).await?;
        self.touch(id);
        Ok(())
    }

    async fn create_standalone_stream(&self, id: &SessionId) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        let stream = self.inner.create_standalone_stream(id).await?;
        Ok(self.track_stream(id, stream))
    }

    async fn resume(&self, id: &SessionId, last_event_id: String) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        let stream = self.inner.resume(id, last_event_id).await?;
        Ok(self.track_stream(id, stream))
    }

    async fn restore_session(&self, id: SessionId) -> Result<RestoreOutcome<Self::Transport>, Self::Error> {
        let _guard = self.create_lock.lock().await;
        if self.inner.has_session(&id).await? {
            self.touch(&id);
            return Ok(RestoreOutcome::AlreadyPresent);
        }
        self.ensure_capacity().await?;
        let outcome = self.inner.restore_session(Arc::clone(&id)).await?;
        if matches!(&outcome, RestoreOutcome::Restored(_)) {
            let _previous = self.activity.lock().insert(id, SessionActivity::at(self.clock.monotonic()));
        }
        Ok(outcome)
    }
}

async fn require_bearer(State(expected): State<Arc<str>>, request: Request, next: Next) -> Response {
    if bearer_matches(request.headers(), &expected) {
        return next.run(request).await;
    }

    tracing::warn!(method = %request.method(), path = %request.uri().path(), "rejected unauthenticated HTTP MCP request");
    (StatusCode::UNAUTHORIZED, [(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))], "Unauthorized").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;

    #[tokio::test]
    #[expect(clippy::expect_used, reason = "test assertion requires the capacity error")]
    async fn session_cap_releases_capacity_after_close() {
        let manager = CappedSessionManager::new(1, Duration::from_mins(1));
        let (first_id, _transport) = manager.create_session().await.unwrap();
        let error = manager.create_session().await.err().expect("second session should exceed the configured cap");
        assert!(matches!(error, CappedSessionManagerError::Capacity(1)));

        manager.close_session(&first_id).await.unwrap();
        let (_replacement_id, _transport) = manager.create_session().await.unwrap();
    }

    #[tokio::test]
    async fn idle_session_is_reaped_and_releases_capacity() {
        let clock = Arc::new(MockClock::new());
        let manager = CappedSessionManager::new_with_clock(1, Duration::from_millis(1), Arc::<MockClock>::clone(&clock));
        let (id, _transport) = manager.create_session().await.unwrap();
        clock.advance(chrono::TimeDelta::milliseconds(1));
        manager.reap_stale().await;
        assert!(!manager.inner.has_session(&id).await.unwrap());
        let (_replacement_id, _transport) = manager.create_session().await.unwrap();
    }
}
