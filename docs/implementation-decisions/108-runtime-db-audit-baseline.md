# Runtime database audit baseline and integrity categories

## Choice

`holon debug runtime-db audit` is the read-only diagnostic surface for stored
projection-effect compatibility and Brief integrity. The library returns a
typed report; the CLI renders that report as human-readable text or JSON.
`--check` selects `projection-effects`, `brief-integrity`, or `all`, and
`--sample-limit` bounds identifiers without returning message or Brief
content.

Brief integrity is reported per Agent:

- A: an operator turn has neither an operator-visible assistant delivery nor
  a canonical Brief;
- B: an operator-visible assistant delivery has no canonical Brief;
- C: a canonical Brief has missing, ambiguous, or invalid `brief_created`
  linkage;
- D: a canonical Brief links below the retained event floor;
- E: the browser received an event but hydration or projection failed, which
  is explicitly not observable from an offline runtime database.

The report includes runtime/database identity, event-log epoch, retained
event floor and head, aggregate counts, and bounded sample identifiers.

## Reason

Existing databases may contain historical rows written before the current
projection-effect and Brief-linkage contracts. Rewriting those rows would
replace evidence rather than diagnose it. Operators instead supply the
deployment boundary with `--baseline-through <RFC3339>`. Findings at or
before that boundary are frozen as `historical_baseline`; later findings are
`new_violation`. With no boundary, findings count as new violations so the
command cannot silently bless history.

The command exits unsuccessfully only when the selected checks contain a
`new_violation`. Historical baseline findings remain visible but do not fail
the audit.

## Preserved boundary

The audit does not update, synthesize, or backfill events, turns, transcript
entries, or Briefs. It uses grouped SQL and bounded samples rather than
scanning the event log once per Brief. It does not advertise observer-sync
capabilities, enable the W6 cutover, or remove legacy event handling.
