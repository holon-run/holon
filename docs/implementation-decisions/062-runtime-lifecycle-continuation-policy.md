# Runtime Lifecycle Continuation Policy

Decision:

- classify every admitted message as either model re-entry or liveness-only
- let only matching, terminal `TaskResult` messages resume task-result waits
- keep non-terminal task updates and mismatched task results liveness-only
- let contentful external events and contentful system ticks resume external
  rechecks
- reserve `ResumeOverride` for operator input and same-work-item terminal task
  results that safely close stale waits
- select continuation anchors by explicit message identity and stable sequence
  ordering, with sequenced operator messages preferred over legacy null
  sequences

Reason:

- task status reduction and scheduler re-entry are separate lifecycle layers
- non-terminal background progress should update projections without spending a
  model turn
- wake hints without content are coordination signals, not new task context
- operator input remains the explicit trust-boundary override for stale waits
- legacy/null message ordering must not make old operator prompts look newer
  than sequenced requests
