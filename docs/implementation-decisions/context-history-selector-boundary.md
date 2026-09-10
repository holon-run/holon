# Context history selector boundary

## Decision

The context projection exposes `recent_turns` and `work_item_scoped` as
request-scoped history selectors. `work_item_scoped` is the default;
`recent_turns` remains an explicit compatibility override and the fallback
when request-scoped provider projection is unavailable.

The baseline prompt keeps the existing agent-recent window. When scoped
projection is adopted, the runtime may query a deeper owner-scoped TurnRecord
window from the same canonical persisted state and hydrate only evidence
referenced by those turns. This is still one request-scoped projection and one
provider request, not a second context build or provider comparison.

## Preserved boundary

Selector evaluation is model-free and must not mutate scheduler, queue,
settlement, replay, audit, or persisted transcript state. Comparing selectors
must not issue a second provider request or compare provider outputs. The
agent-recent baseline remains isolated from the deeper scoped evidence so
fallback cost does not grow. Owner-scoped output remains subject to the same
turn projection token budget, and projection diagnostics record the selected
window, fallback or no-op outcome, and evaluated safety invariants.
