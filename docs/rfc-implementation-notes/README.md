# RFC Implementation Notes

These notes summarize implementation status and follow-up work across related
RFCs. They are intentionally non-normative:

- RFC contracts live under `docs/rfcs/`.
- The cross-RFC status table lives in
  [`../rfc-implementation-matrix.md`](../rfc-implementation-matrix.md).
- Durable design choices live under `docs/implementation-decisions/`.

Use this directory when a topic spans several RFCs and the implementation state
would be noisy or misleading if duplicated inside each RFC.

## Current notes

- [control-plane-and-delegation](control-plane-and-delegation.md)
- [event-and-operator-transport](event-and-operator-transport.md)
- [event-timeline-projection-audit](event-timeline-projection-audit.md)
- [memory-and-compaction](memory-and-compaction.md)
- [policy-and-execution-boundary](policy-and-execution-boundary.md)
- [projection-query-and-subscription-api](projection-query-and-subscription-api.md)
- [recent-turns-context-spine](recent-turns-context-spine.md)
- [runtime-database-storage-migration](runtime-database-storage-migration.md)
- [runtime-scheduler-contract](runtime-scheduler-contract.md)
- [tool-contracts](tool-contracts.md)
- [work-items-and-waiting-plane](work-items-and-waiting-plane.md)

## Maintenance rules

1. Keep notes short and implementation-facing.
2. Link back to RFC handles from the matrix instead of inventing parallel names.
3. Prefer implementation anchors, verification anchors, and open gaps over
   restating RFC requirements.
4. Update the matrix when a note changes a status, anchor, or gap.
5. If a note records a durable architectural choice rather than status, promote
   that choice into `docs/implementation-decisions/` and link it from the
   matrix.
