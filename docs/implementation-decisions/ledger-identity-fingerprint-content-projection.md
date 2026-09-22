# Ledger identity fingerprint projects immutable content

The Web GUI event ledger stores one durable record per canonical identity
`(event_log_epoch, agent_id, event_seq)` and refuses to overwrite it when a
redelivery carries different immutable content. That guard compares a
fingerprint of the stored envelope with a fingerprint of the incoming one.

The fingerprint is now computed over a fixed projection of the event's
durable identity only: `agent_id`, `event_log_epoch`, `event_seq`, `id`,
`ts`, and `type`. Envelope-level transport metadata (`contract_version`,
`provenance`, `payload_schema`/`payload_schema_version`,
`projection_effect`) and `payload` are deliberately excluded.

The reason: those fields are declared by the transport contract, not by the
event. The public event contract declares the contract version once per stream,
derives `provenance` from the payload, and emits `payload_schema` only for
typed events, so an envelope contract revision legitimately adds or drops them
without changing the event. Fingerprinting the whole envelope made every such
revision look like an identity conflict, and because the fingerprint was also
*persisted*, a client upgrade could not re-derive a matching value from the
old stored envelope. The guard therefore recomputes the stored fingerprint from
the stored envelope with the current projection instead of trusting the
persisted string, so a projection change converges instead of invalidating the
cache.

`payload` belongs to that same transport-projected surface, not to the durable
event. The public event contract publishes a projection of the durable payload
rather than the durable payload itself: `public_event_payload()` in the daemon
event endpoints removes provider diagnostics (`context_fingerprint`,
`compression_epoch`, `prompt_cache_key`, `provider_request_diagnostics`,
`provider_attempt_timeline`, and similar) while keeping them in the durable
audit record. A row stored under an earlier contract revision therefore
legitimately differs from its redelivery, and keeping `payload` in the
projection reproduced the same false-conflict class that this decision fixed
for envelope metadata: the conflict aborts the ingestion transaction, so the
stored row never converges and the error repeats on every bootstrap.

Preserved boundary: a genuine identity collision still hard-fails. The
canonical identity `(event_log_epoch, agent_id, event_seq)` plus the durable
`id`, `ts`, and `type` are still compared, so a redelivery that carries a
different event under the same canonical identity still raises
`LedgerIdentityConflictError` and never silently overwrites the stored value.

Residual: a duplicate redelivery keeps the stored envelope, including the
payload projection that was current when it was ingested. The ledger stores the
event as it was published, and does not re-project stored rows on a later
contract revision.
