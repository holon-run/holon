# Provider Retry Classification

Decision:

- classify transport failures into retryable and fail-fast buckets
- retry transient failures at most two times before moving to the next fallback
- keep retry policy visible in diagnostics

Reason:

- some provider failures are recoverable on the same path
- deterministic contract or auth failures should not burn retries
