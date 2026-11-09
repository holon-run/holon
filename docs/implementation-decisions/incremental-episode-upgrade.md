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
and rollback. The same availability rule applies to ordinary current reads,
Task/Input/Scheduling capture, resident views, and writer preparation: cumulative
bytes, records and decoded nodes are observations, not admission quotas. Complete
closure queries must not truncate at a former quota. Explicitly requested
page/window sizes remain selection semantics. No historical payload rewrite is
required.

The fixed Task Message inventory also accepts an exact RFC3339 timestamp from
older SQL rows when it denotes the same instant as the Message body. The existing
millisecond projection remains valid. This preserves the original stored
fingerprint and payload; a different sub-millisecond instant is still rejected.
Offline upgrade verification exposed this compatibility issue only after the
cumulative history rejection was removed.

A fixed upgrade can retain a completed Tool trace whose Work ID names another
agent's completed Work. Treat that ID as a historical annotation only when the
Tool is nondeferred, its own AgentLifecycle Turn is terminal with no Work owner,
and the Turn is not current or bound to the current execution. Retain the Tool
and Turn evidence without adopting the foreign Work. This exception is confined
to the original upgrade; ordinary closures and independent current ownership
claims still reject foreign Work IDs.

Startup recovery reserves registered scopes, original fixed seeds, and unfinished obligations. Closed Queue/Turn history without a registered scope or original seed remains history; its presence alone cannot require or mint recovery authority. Live orphan obligations still fail closed when their fixed seed is missing.
