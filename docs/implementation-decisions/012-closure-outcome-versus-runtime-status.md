# Closure Outcome Versus Runtime Status

Decision:

- keep `AgentStatus` as runtime control and posture state
- derive a separate closure view for `completed`, `continuable`, `failed`, and
  `waiting`
- keep semantic waiting reason separate from sleeping posture

Reason:

- runtime control flow and operator-facing closure meaning are different layers
- blocking tasks should remain visible without automatically becoming
  critical-path waiting
- runnable persisted work should not be collapsed into `completed`
