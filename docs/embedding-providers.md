# Embedding Providers

LocalHold uses one OpenAI-compatible HTTP contract for local and cloud
embeddings. It does not start or manage the model service. Memory content is
sent to the selected endpoint when memories are indexed, and search queries are
sent when semantic retrieval runs. The
[security, privacy, and threat model](security-and-privacy.md) describes the
provider boundary, stored vectors, error logging, and fully local alternative.

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

Successful embedding response bodies are limited to 16 MiB, and model-list
response bodies are limited to 1 MiB. LocalHold enforces these limits while
streaming even when the provider omits `Content-Length`. Provider HTTP error
bodies are discarded; runtime errors retain the HTTP status and retry
classification without returning provider-controlled body text.

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

The HTTP client honors system proxy environment variables, including
`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, and lowercase equivalents.
A proxy can therefore receive embedding inputs and API key headers. Audit the
server process environment and use `NO_PROXY` for loopback or private endpoints
that must not cross that additional boundary.

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
variables are `LOCALHOLD_EMBEDDING_MAX_RETRIES`,
`LOCALHOLD_EMBEDDING_RETRY_INITIAL_BACKOFF_MS`, and
`LOCALHOLD_EMBEDDING_RETRY_MAX_BACKOFF_MS`.

`limits.max_concurrent_embedding_requests` bounds simultaneous provider calls
across semantic queries, health checks, background indexing, batch requests,
and recovery work. Lower it for constrained local hardware or strict
hosted-provider quotas; raise it only after measuring model capacity and
rate-limit behavior.

`remember_many` and bulk/startup re-embedding send explicit provider batches
of up to `limits.embedding_batch_size` texts. Separate chunks may run
concurrently, subject to `limits.max_concurrent_embedding_requests`. Single
writes, updates, and semantic search queries remain immediate and are never
held for microbatching. If a provider permanently rejects a batch or returns
the wrong number of vectors, LocalHold retries that chunk one input at a time
so one invalid record does not block valid records. Transient and rate-limit
failures are not expanded into individual requests.

Concurrency and retry limits are per LocalHold process. When several instances
share a database and one hosted endpoint, size
`max_concurrent_embedding_requests` for their aggregate traffic; LocalHold does
not provide a distributed provider-rate limiter. Durable re-embedding claims
prevent instances from selecting the same pending revision during normal bulk
recovery. Revision-checked writes remain the final guard if a lease expires or
an immediate write races with recovery, so duplicate inference is possible but
a stale vector cannot replace a current one.

To compare explicit batch sizes against the local store and task scheduler, run
`cargo bench --bench embed_throughput`. The benchmark includes chunk sizes 1
and 32 across several workload sizes with fixed simulated request latency.
Provider-specific network and model latency should still be measured in the
intended deployment before changing the defaults.

Changing the endpoint, model, or dimensions changes the stored vector-space
identity. Follow the reindex procedure in [Operations](operations.md) before
starting LocalHold with the new identity.
