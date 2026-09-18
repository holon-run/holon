# Ledger cursor regression merges monotonically

## Choice

`EventLedger` agent-session cursor fields (`ingestedThroughSeq`,
`observedHeadSeq`, `projectionReadyThroughSeq`) merge monotonically against
the value observed inside the same transaction: a batch that requests a
lower cursor than the stored one commits with the stored value instead of
aborting. `LedgerCursorRegressionError` is now reserved for a patch whose
own fields are inconsistent (`projectionReadyThroughSeq` claiming past
`ingestedThroughSeq`), which is always a programming error.

## Reason

Concurrent writers share one IndexedDB: a snapshot install can commit a
higher cursor while an ingest batch computed from a stale tracker is in
flight, within one page (snapshot install vs in-flight ingest) or across
tabs. Rejecting the stale batch aborted the whole transaction, discarding
its idempotent raw events and surfacing `Agent ledger ingestion failed`
console warnings during bootstrap/backfill-heavy windows. Observed in
production four times (2026-09-12, 2026-09-14, 2026-09-18 x2), always at
bootstrap or force-backfill moments.

## Preserved boundary

Cursor fields remain monotonic by construction; the merge keeps the higher
value and never moves a cursor backwards. Identity conflicts on raw events
still hard-fail the transaction unchanged.
