# visibility_scope_id derivation inputs

Source: `docs/rfcs/observer-sync-agent-summary-and-read-markers.md` (S1
identity slice). `visibility_scope_id` is derived as a SHA-256 of
`vscope1` domain separator, the runtime installation id, the server-resolved
authority principal, the normalized visibility entitlement, and the
`visibility_policy_generation` counter.

Credential material is never an input. Rotating the control token with the
same principal and entitlement therefore keeps the scope stable, while a
principal, entitlement, policy-generation, or runtime-identity change rotates
it. Local unauthenticated mode uses the fixed runtime-local `public` scope
(`public`/`public`).

S1 ships the pure derivation in `src/ids.rs` plus the durable policy
generation in `runtime_metadata`; per-request authority resolution and the
roster/projection snapshot fields that serve the scope arrive with the S4
snapshot slice. The S1 foundation verification derives the public scope twice
and records a fingerprint, proving determinism across reopens without
exposing the value through any HTTP contract early.
