# Index and event payload disposition

## Decision

Secondary indexes and audit events should not carry full duplicate runtime
objects when a canonical table or ledger entry already owns the object. Keep
index/event payloads to stable ids, summaries, and bounded display metadata.

## Disposition

- `message_search_index`: removed the `payload_json` FTS column. The index now
  stores only ids, filter fields, kind, and searchable body text; query results
  hydrate from the canonical `messages` table.
- `work_item_plan_artifact_refreshed`: replaced the full `plan_artifact` copy
  with artifact path, hash, byte count, update time, and preview completeness.
  The work item record and plan artifact file remain canonical.
- `work_item_continuation_*`: replaced full continuation frames in event
  payloads with continuation summaries. The `work_item_continuations` table
  remains canonical.
- `turn_record`: kept the event as an auditable notice with turn ids and
  relation ids. The `turn_records` spine remains canonical for the full turn
  projection.
- `turn_terminal_aborted`: removed the embedded terminal record copy because
  the terminal record is already persisted through the turn terminal and turn
  record paths.

Canonical `payload_json` columns remain in object-owner tables where they are
the durable object representation rather than a secondary duplicate.
