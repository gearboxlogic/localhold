# Embedding Providers

LocalHold uses one OpenAI-compatible HTTP contract for local and cloud
embeddings. It does not start or manage the model service. Memory content is
sent to the selected endpoint when memories are indexed, and search queries are
sent when semantic retrieval runs.

## Request Contract

The configured `base_url` is joined with `embeddings` and, by default, with
`models` for health checks. For example, `https://example.test/v1` produces
`POST https://example.test/v1/embeddings` and
`GET https://example.test/v1/models`.

Every embedding request sends `model`, `input`, and
`encoding_format = "float"`. Set `send_dimensions = true` only when the selected
provider and model accept a `dimensions` request field. LocalHold always checks
that returned vectors match `[embedding].dimensions`, regardless of whether the
field is sent.

Responses must use the OpenAI-compatible shape below. Batch responses may be
out of order; LocalHold restores input order using each item index.

```json
{
  "data": [
    { "index": 0, "embedding": [0.1, 0.2, 0.3] }
  ]
}
```

## Authentication

`auth_mode = "bearer"` sends `Authorization: Bearer <api_key>` and is the
default for OpenAI-compatible services. `auth_mode = "api_key"` sends the same
credential in the Azure-compatible `api-key` header. Omit `api_key` for a local
endpoint that does not authenticate requests.

LocalHold never follows HTTP redirects from the configured endpoint. This
prevents an endpoint credential from being forwarded to a different origin.

## Health Checks

`health_check = "models"` verifies that `GET /models` succeeds and lists the
configured model. This is the default and gives outage recovery a real probe.

Use `health_check = "disabled"` when a compatible cloud service exposes
embeddings but not model listing. In this mode startup assumes the endpoint is
available, and embedding requests provide the first real availability signal.
After an outage, the periodic probe permits another request cycle rather than
calling an unsupported health route.

## Transport Security

HTTPS is required for non-loopback endpoints. Plain HTTP remains available for
`localhost` and loopback IP addresses so local vLLM, llama.cpp, or Ollama
services work without TLS.

For a trusted private network that intentionally uses plaintext HTTP, set
`allow_insecure_http = true`. This exposes memory content, queries, and API keys
to that network and should not be used across an untrusted boundary.

## Failure Behavior

Transient network failures, timeouts, HTTP 408, and server errors are retried
with exponential backoff and jitter. The availability circuit opens only after
those attempts are exhausted. LocalHold then continues with keyword and text
retrieval while periodically checking for recovery. Authentication,
request-shape, and response-shape failures are treated as permanent and are not
retried.

HTTP 429 is tracked separately from provider outages and never opens the
availability circuit. LocalHold honors `Retry-After` seconds and HTTP dates as a
minimum delay. It returns the rate-limit error immediately when the provider's
requested delay exceeds `limits.embedding_retry_max_backoff_ms`, allowing the
caller to decide when to try again without holding a task indefinitely.

`limits.embedding_max_retries` controls retries after the initial request and
may be set to zero. `limits.embedding_retry_initial_backoff_ms` controls the
first client delay; later delays double up to
`limits.embedding_retry_max_backoff_ms`. The corresponding environment
variables are `RECALL_EMBEDDING_MAX_RETRIES`,
`RECALL_EMBEDDING_RETRY_INITIAL_BACKOFF_MS`, and
`RECALL_EMBEDDING_RETRY_MAX_BACKOFF_MS`.

`limits.max_concurrent_embedding_requests` bounds simultaneous provider calls
across semantic queries, health checks, background indexing, batch requests,
and recovery work. Lower it for constrained local hardware or strict
hosted-provider quotas; raise it only after measuring model capacity and
rate-limit behavior.

Changing the endpoint, model, or dimensions changes the stored vector-space
identity. Follow the reindex procedure in [Operations](operations.md) before
starting LocalHold with the new identity.
