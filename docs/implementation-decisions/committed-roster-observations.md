# Keep complete roster observations independent of execution readiness

`/agents/list` keeps the last successfully published lightweight projection for
each identity in the mutation-owner slot. The capture returns that projection
and its version under the publication mutex. Ordinary State commits and known
publication recovery do not discard it; the wire reports the pending status.
Execution capture keeps its existing Ready/reconciliation fence.

This reuses the existing memory product rather than rebuilding every list from
SQLite or serving an unmarked HTTP fallback. It preserves a complete identity
set without making a slow writer an availability gate for the whole roster.
The retained product holds list fields and their budget guard, not a full
resident State/Work/Task graph. A failed refresh retains at most its preceding
valid projection; background repair remains responsible for replacement.

Identity-changing metadata writes close observation before their SQL attempt.
Unknown identity outcomes, changed revisions, missing initial publications and
incarnation mismatches remain unavailable. A previous observation is not
permission to discover a revoked identity or to execute against an old version.
The additive observation fields are optional when decoding older servers and
are not execution tokens. Full State/context reads keep their stricter contract.
