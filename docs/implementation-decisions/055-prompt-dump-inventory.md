# 055 Prompt Dump Inventory

## Choice

Prompt debug dumps include a small section inventory before rendered prompt
content. The inventory reports system/context section counts, stability counts,
rendered character counts, per-section order, ids, stability, cache-scope
classification, and content size.

## Reason

Prompt assembly bugs are usually boundary bugs: a section is in the wrong layer,
has the wrong stability, moved unexpectedly, or appears to be cache-scoped when
it is actually turn-only. A dump should make those boundaries visible without
requiring provider-specific request inspection.

## Boundary

This is an inspectability contract only. The inventory must not feed back into
prompt assembly, provider lowering, cache keys, scheduling, or runtime behavior.
