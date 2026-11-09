# Capture legacy continuity incrementally

Migration 94 must preserve the complete original continuity cut even when a
long-lived agent's cumulative history exceeds 64 MiB or 65,536 records. These
cumulative thresholds are not memory bounds: the referenced bodies can be read,
validated and hashed independently. Rejecting their sum prevents the entire
runtime from starting without reducing its persistent history.

Visit root rows and Episode bodies incrementally in the existing migration
transaction. Keep the existing identity, content digest, receipt exclusion,
live-owner and explicit-reference qualification checks. Select the twelve recent
unreceipted Episodes by decoded timestamps and ID, exactly as before. Only the
candidate ID/digest inventory and required ownership metadata grow with history;
full historical bodies are not retained together. This is not a constant-memory
claim: the existing seed JSON still contains every candidate digest.

Remove cumulative body/count rejection from the Episode/Work upgrade closure
and its Wait relation inventory; do not replace it with a larger threshold.
Retain reference deduplication and depth validation, fixed transaction cutover,
rollback, and the separate bounds on ordinary current reads. No schema or
historical payload rewrite is required. Task/current-read domains retain their
separate contracts.

The fixed Task Message inventory also accepts an exact RFC3339 timestamp from
older SQL rows when it denotes the same instant as the Message body. The existing
millisecond projection remains valid. This preserves the original stored
fingerprint and payload; a different sub-millisecond instant is still rejected.
Offline upgrade verification exposed this compatibility issue only after the
cumulative history rejection was removed.
