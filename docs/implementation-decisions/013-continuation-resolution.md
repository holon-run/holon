# Continuation Resolution

Decision:

- derive typed continuation from prior closure, trigger kind, and trigger
  contentfulness
- treat blocking `TaskResult` as the canonical delegated-work rejoin point
- keep `SystemTick` as `liveness_only`

Reason:

- wake, queue ingress, and model-visible continuation are different layers
- explicit continuation resolution makes follow-up behavior inspectable
