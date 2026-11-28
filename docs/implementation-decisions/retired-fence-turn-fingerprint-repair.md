# Retired destructive-operation fence Turn fingerprint repair

Issue: #4668

## Choice

Migration v123 `repair_retired_destructive_operation_turn_fingerprints`
re-freezes only Turn input-evidence fingerprints whose drift is attributable
to the v121 fence rewrite: the current `turn_records.payload_json` no longer
carries `$.destructive_operation_id`, and every non-body fingerprint field
(`turn_index`/`created_at`/`owner_kind`/`owner_id`/`revision_marker`/null
projections) still matches the row. Drifted rows that fail that shape are
counted, logged, and left unchanged; the migration completes instead of
aborting. Counters land in `runtime_metadata` under the migration name so the
report survives log rotation.

## Reason

The production failure blocked agent runtime construction and pinned queued
inputs for every turn the v121 rewrite touched, so the repair must ship in the
upgrade path. Aborting on an unrelated anomaly would keep those agents
quarantined for a defect they do not have: read-time qualification remains the
backstop that keeps un-repaired drift visible, so "not attributable" rows stay
broken-and-observed rather than migrated-and-legalized. Repairs follow the
v119/v115 repair precedents: `input_evidence_current` stores the body digest in
its own column and demands full fingerprint equality, while
`input_evidence_eligibility` and `input_evidence_seed_recovery_current` (the
`input_evidence_seed_membership` view) freeze the digest inside the
fingerprint. Scans run in cursor-batched `LIMIT 128` keyset passes and re-run
as no-ops once fingerprints match the row.

## Preserved boundary

- Only `family = 'turn'` rows are touched; work_execution, outcome, and other
  families keep their v116/v119-repaired shapes untouched.
- Fingerprints whose metadata differs from the row, payloads that still carry
  a live fence value, and malformed JSON are never rewritten.
- Compacted baselines without `turn_records` skip the scan entirely, and
  fresh databases repair nothing.
