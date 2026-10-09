# Subagent reclamation reads existing durable facts

## Choice

Use the existing deletion coordinator for bounded keyset observation, one owned
deletion worker, one reminder worker, and one idle retirement. Read canonical
supervision plus existing work/state projections; persist only changed candidate
observations and reminder outbox transitions. Reuse normal internal-message
admission and queue idempotency rather than interpreting a model review reply.

## Reason

A separate event consumer and monitor agent would duplicate lifecycle authority
and require crash-safe replay of another projection. Existing durable facts are
sufficient for advisory discovery and transactional deletion admission. Periodic
keyset fallback eventually visits objects beyond the first batch and does not
need transcript or descendant-tree scans. Discovery latency grows with fleet size;
domain events can accelerate it later without becoming the source of authority.

## Preserved boundary

Activity observations grant no deletion authority. The current parent requests
deletion, and the runtime revalidates protection in a serialized admission.
Background switches stop new admissions; accepted deletion remains recoverable.
Instance retirement closes old execution handles and joins actual owned work,
preserving identity/history; it stays opt-in until deployment measurements pass.
