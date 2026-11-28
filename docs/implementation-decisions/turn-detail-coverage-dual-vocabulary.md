# Turn detail coverage uses two closed vocabularies

## Decision

Compute a turn's detail coverage from two independent closed vocabularies instead
of one string union: the activity `source_kind` registry and the
`TranscriptEntryKind` domain enum. An unknown `source_kind` reports
`unknown_activity_type`; a known source whose canonical evidence row is retained
away reports `retention_gap`; a transcript kind the domain enum cannot represent
reports `unknown_activity_type`, while known detail kinds such as `tool_results`
report nothing.

## Reason

Transcript entries and activity sources model different things. Normal turns
write `tool_results`, `incoming_message`, `continuation_prompt`, and
`subagent_prompt` rows without registering an activity source, so treating every
non-assistant transcript kind as unmodeled made coverage permanently `partial`
for any turn that used a tool (#4665). Keeping the vocabularies separate keeps
the guard against genuinely unmodeled rows without turning known detail rows into
user-visible degradation.

## Preserved boundary

No public enum, wire value, client rendering, or migration changes. Transcript
kinds keep one exhaustive `match` that forces each new variant to state whether
it backs an activity source, and the coverage check decodes stored kinds through
the domain serde contract instead of a second hand-maintained list. Page
fallbacks never weaken a turn-level reason that already outranks them, and
`retention_gap` keeps priority over `unknown_activity_type`.
