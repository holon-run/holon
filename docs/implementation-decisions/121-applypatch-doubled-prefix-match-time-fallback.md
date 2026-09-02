# ApplyPatch: doubled diff-marker prefixes reinterpreted at match time only

## Choice

When a hunk's context block fails both strict and relaxed matching, ApplyPatch
retries once with doubled diff-marker prefixes stripped from the hunk lines
(`-- text` as Remove of `text`, `+- text` / `++ text` as Add of `text`). A
successful retry applies the patch and records a `double_prefix_reinterpreted`
diagnostic; otherwise the original `context_not_found` error is returned
unchanged.

## Reason

Codex-DSL models intermittently emit doubled markers (#2439), which parse as
removal/addition of `- text` and then fail to match. Reinterpretation must not
happen at parse time: `-- item` is the legitimate spelling for removing a
Markdown list item `- item`, and parse-time stripping would silently corrupt
those edits. Match-time fallback keeps strict semantics first-class.

## Preserved boundary

The alternative is only tried when the strictly parsed block matches nothing,
and both Remove and Add lines are reinterpreted together so one hunk is not
half-corrected. If the file genuinely contains the marker-initial text, the
strict match wins and no diagnostic appears.
