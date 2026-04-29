# Work-Item Reactivation Uses Continuable Closure

Decision:

- derive `ClosureOutcome::Continuable` when runnable persisted work remains
  after a turn and no higher-priority blocker applies
- populate `ClosureDecision.work_signal` from persisted work-item state
- keep waiting-plane ownership of blocked reactivation reasons

Reason:

- runnable work and blocked work are different operator-visible states
- the runtime already reactivates from persisted `active` and `queued` work
  items, so closure should expose that truth instead of collapsing it into
  `completed`
- keeping `waiting` reserved for real blockers avoids teaching the model that
  every unfinished work item is a wait
