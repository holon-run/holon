# Context history selector boundary

## Decision

The context projection exposes `recent_turns` and `work_item_scoped` as
request-scoped history selectors. Both selectors are evaluated from the same
`EffectivePrompt`/canonical runtime snapshot and produce diagnostics and
manifests; only one projection is sent to a provider.

## Preserved boundary

Selector evaluation is model-free and must not mutate scheduler, queue,
settlement, replay, audit, or persisted transcript state. Comparing selectors
must not issue a second provider request or compare provider outputs. The
existing `recent_turns` projection remains the compatibility default while
`work_item_scoped` is introduced as an observable candidate for later,
independently gated rollout.
