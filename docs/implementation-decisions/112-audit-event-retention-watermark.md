# Audit-event retention uses a separate durable floor

The committed event head remains `runtime_sequences.last_value`. Retention
stores its hard recovery boundary separately in
`audit_event_retention_watermarks`, keyed by the same `agent:<id>` or `host`
scope key.

An actual prefix deletion through `D` advances the floor to `MAX(existing,
D + 1)` in the delete transaction. No row means `0`: no deletion is known to
have occurred. This keeps allocation and deletion provenance distinct, keeps
the floor monotonic when no event survives, and lets recovery reads avoid
aggregating the retained event ledger.
