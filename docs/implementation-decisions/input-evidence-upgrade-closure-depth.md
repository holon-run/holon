# Frozen Input evidence eligibility closure converges instead of failing

Issue: #4429

## Choice

The frozen capture boundary walks the metadata lineage closure to
convergence. `metadata_closure` in
`crates/holon-runtime/src/runtime_db/input_evidence_snapshot.rs` no longer
carries a depth counter, and the capture no longer fails with
`ResourceExhausted("eligibility closure depth")`.

This applies to every phase that calls `capture_input_evidence_seed_tx`
(`upgrade`, `first_import`, `legacy_import`). Those phases freeze one
checkpoint of qualified records, so a partial closure would silently drop
records from `input_evidence_eligibility` and would make
`input_evidence_captures.metadata_digest` a function of an arbitrary cutoff
instead of a function of the frozen data.

## Reason

Migration 96 (`qualified_input_evidence`) runs under the migration lease on
real databases. The 64-hop depth cap made the migration fail closed on every
database whose typed lineage is deeper than that, which left v0.61.0 nodes in
a restart loop with `schema_migrations` stuck at 95 and the daemon serving
502. A historical depth limit is not evidence of corruption, so it must not
be a reason to abort a frozen upgrade.

The closure is metadata-only and each key is visited once, so the walk stays
bounded by the number of distinct typed keys and never reads body payloads
outside the fixed digest set.

## Preserved boundary

Only the frozen capture path changed. Online reads keep their own strict
limits and still fail closed:

- `prepare_input_records_tx` keeps the reference depth and provider lineage
  depth limits.
- Authorized task-output recall keeps its exact output depth limit, because
  truncating it would break the authorization semantics of the recall.
- The work-domain loader keeps its closure depth limit on the online read
  path.

Online reads over data that is deeper than those limits can still fail with
`ResourceExhausted`. That is a separate, non-blocking follow-up: it does not
abort an upgrade.

The upgrade capture also keeps its fail-closed checks. A damaged or
inconsistent record still fails the migration, and a readback mismatch of a
written fingerprint still fails the capture.
