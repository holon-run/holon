# Ledger identity fingerprint projects immutable content

The Web GUI event ledger stores one durable record per canonical identity
`(event_log_epoch, agent_id, event_seq)` and refuses to overwrite it when a
redelivery carries different immutable content. That guard compares a
fingerprint of the stored envelope with a fingerprint of the incoming one.

The fingerprint is now computed over a fixed projection of the event's
immutable content only: `agent_id`, `event_log_epoch`, `event_seq`, `id`,
`ts`, `type`, and `payload`. Envelope-level transport metadata
(`contract_version`, `provenance`, `payload_schema`/`payload_schema_version`,
`projection_effect`) is deliberately excluded.

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

Preserved boundary: genuine content divergence still hard-fails. The projection
keeps `id`, `ts`, `type`, and `payload`, so a redelivery with different event
content for the same canonical identity still raises
`LedgerIdentityConflictError` and never silently overwrites the stored value.
The projection only stops conflating the transport wrapper with the event.
