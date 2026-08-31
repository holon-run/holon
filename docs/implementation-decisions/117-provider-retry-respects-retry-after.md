# Provider Retry Respects Retry-After

Decision:

- parse the standard `Retry-After` header (RFC 9110 delta-seconds or HTTP-date)
  at the transport boundary into a `Duration` before classifying status errors
- carry it on `ProviderTransportError.retry_after` and, in the retry loop,
  wait `max(server_hint, computed_backoff)` for 429/503 failures
- cap honored server hints at 30s; a hint above the cap skips the remaining
  in-provider retries and defers to the existing fallback path without sleeping
- record `backoff_source` (`server_retry_after` | `computed_backoff`) on each
  `ProviderAttemptRecord` so timelines show where the effective backoff came from

Reason:

- fixed 200ms/400ms backoff retried inside a rate-limit window that the server
  already quantified (observed 1-13s on the Codex backend), so every retry
  failed and forced premature fallback
- waiting a bounded server hint is cheaper than burning a fallback model
- hints above the cap are treated as "this provider is unavailable for now";
  deferring to fallback keeps turn wall-clock bounded

Preserved boundary / tradeoff:

- classification stays provider-agnostic: call sites pre-parse headers into
  `Option<Duration>`, leaving room for body-encoded hints (e.g. Gemini
  `RetryInfo.retryDelay`) to flow through the same field without touching the
  decision layer
- no jitter is applied to a server hint because it encodes server-side
  throttle state, not concurrent-retry contention
- vendor-specific reset headers (`x-ratelimit-*`, `anthropic-ratelimit-*`)
  remain follow-up work
