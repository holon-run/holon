# Event-page kind selection keeps a disjoint fallback range

`audit_events.kind` is written alongside the canonical `AuditEvent` JSON, but
historical rows and externally modified databases are not proof of agreement.
Filtering only the normalized column can lose a matching canonical event or
hide a malformed body.

The Event-page index therefore uses a derived key: normalized kind only when
JSON is valid and its typed kind, id, and sequence agree with the row; otherwise
NULL. Known `AuditEvent` fields must also be unique: removing them twice must
be idempotent. Unicode escapes conservatively use fallback because SQLite
does not resolve escaped field names like serde, which rejects duplicates.
The migration and query share the exact SQL expression. A filtered page
merges two disjoint, sequence-ordered index ranges: the requested key and the
NULL fallback. This avoids scanning or parsing every unrelated body while
retaining legacy, mismatch, and corruption handling. Index updates automatically
reclassify changed rows; no read-time repair or body qualification is performed.

Every selected candidate still uses the original material accounting, hash,
canonical decoder, address/sequence validation, and canonical-kind filter.
The key is an optimization, not authority to read or return a body. Exact active
identity/incarnation checks, recovery metadata, and the mutation-owner
reservation remain unchanged.

Bounded history selection is explicit. Existing internal historical queries and
the SSE initial buffer do not opt into the new paging scan contract; their
completeness and stream-reconciliation behavior must not change as a side effect
of bounding the public Events history page.
