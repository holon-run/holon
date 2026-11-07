# Transition Commit Future State Boundary

Decision:

- keep `RuntimeHandle::apply_transition_commit` as a thin wrapper and box the
  notification implementation it delegates to
- install committed State and queued sources together under the original
  `RuntimeMutation` reservation, before releasing it for notifications
- do not insert an extra async layer behind a future that many runtime call
  sites await inline

Reason:

- `apply_transition_commit` is awaited inline by the turn execution, message
  dispatch, task state reduction and queue settlement paths
- an awaited nested future contributes its own state to every caller's future,
  and those caller futures are materialized on the runtime stack
- adding the guarded implementation inline grew each caller future by about
  1.2 KiB, which pushed the deepest runtime test chain past the default 2 MiB
  thread stack; the chain then aborted with a stack overflow instead of
  reporting its assertion result
- boxing the delegated future keeps notification state on the heap, so callers
  retain a small future; pause projection belongs to the original mutation,
  not a second raw agent-lock projection after its commit
