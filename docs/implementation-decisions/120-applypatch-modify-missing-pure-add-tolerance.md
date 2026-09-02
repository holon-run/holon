# ApplyPatch: missing modify target with pure-add hunks is tolerated as create

## Choice

A `--- a/path` / `+++ b/path` patch whose target file does not exist is applied
as file creation when **every** hunk is purely additive (only `+` lines, no
context or `-` lines, e.g. `@@ -0,0 +1,N @@`). The changed-file receipt is
marked `A` and carries a `modify_to_missing_treated_as_add` diagnostic so the
reinterpretation stays observable.

## Reason

Models frequently spell new-file patches with `a/` headers instead of
`/dev/null` (#2662: 19 `missing_file(update)` failures across 10 agents). The
result of applying pure-add hunks to an absent file is exactly an add, with no
information loss, mirroring the existing add-branch tolerance for an existing
empty target.

## Preserved boundary

Hunks that reference old content (context or remove lines), delete, and rename
operations against missing targets still fail with `missing_file`; silently
fabricating their results would hide real errors. Path guards (workspace
escape, `a/`-prefixed absolute paths) are evaluated before this tolerance and
are unchanged.
