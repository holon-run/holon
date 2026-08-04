# Provider Retry Classification

Decision:

- classify transport failures into retryable and fail-fast buckets
- retry transient failures at most two times before moving to the next fallback
- keep retry policy visible in diagnostics
- classify a successfully completed wire response with no content/output items as
  `empty_response + retryable`
- keep non-empty responses whose items cannot be mapped to supported model blocks
  as `invalid_response + fail_fast`
- preserve token usage reported by failed responses in the provider attempt
  timeline

Reason:

- some provider failures are recoverable on the same path
- deterministic contract or auth failures should not burn retries
- an empty completed response is an upstream availability failure, not evidence
  that the response contract is unsupported
- wire-item presence, rather than the final mapped block count alone, preserves
  the distinction between transient emptiness and deterministic incompatibility
