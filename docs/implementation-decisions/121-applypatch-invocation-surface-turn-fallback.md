# ApplyPatch: invocation-time surface follows the turn-local fallback binding

## Choice

`current_apply_patch_surface()` resolves the ApplyPatch surface from the
agent state **plus the turn-local fallback binding** (`turn_fallback_model`),
not from the primary route chain head alone. This mirrors how view-image and
generate-image selection already consume the same binding.

## Reason

Provider fallback is bound to recovery messages (#2492): when a lineage
fails, the runtime defers to a recovery turn that starts on the fallback
model, and turn-start tool selection already computes the ApplyPatch schema
for that model. But invocation-time patch parsing used
`provider_chain_for_turn(model_override, None)`, so a patch authored under the
fallback model's schema (for example Codex DSL freeform) was parsed with the
primary model's surface (unified diff JSON). The mismatch surfaced as
compatibility-fallback rescues with misleading diagnostics and wrong receipt
surface labels (#2435).

## Preserved boundary

Model switches happen only at turn boundaries: `FallbackProvider` does not
advance to the next candidate mid-request; it fails the round and the runtime
queues a recovery turn. Per-round surface re-evaluation and mid-turn tool
schema swaps therefore have no reachable trigger and are deliberately not
implemented. The binding is cleared by the existing
`reconfigure_provider_for_turn(None)` turn-end cleanup, so it never leaks
across turns.
