# Operator Abort Is A Durable Negative Replay Decision For Its Exact Input

Decision:

- an operator current-run abort persists an abort intent (agent, identity
  incarnation, run, cancellation lease, attempt, exact source message, Turn)
  atomically with the Stop/Idle posture projection, before the abort ACK and
  before the in-process future is cancelled
- the aborted input settles as terminal `Aborted` (queue entry and Turn
  terminal kind) with reason `operator_aborted`; it is never replayed by
  Start, daemon restart, bootstrap recovery, exact task/wait reentry, or
  debug recovery; retrying the work requires explicitly resubmitting input
  with a new message identity
- queue claim and execution-protocol admission reject a fenced source inside
  their commit transactions, and an interrupted replay source whose attached
  Turn carries `operator_aborted` fails closed as an invalid replay source
  instead of replaying
- terminal commit transactions also recheck the fence; a losing producer's
  prepared completion, wait registration, or provider recovery is not committed
- cancellation settles the attempt with an owner-specific `Aborted` outcome,
  clears its replay anchor without closing or pausing the WorkItem, and settles
  admitted task-result consumption as `InvalidOrStale`
- startup recovery settles an open aborted Turn as `Aborted` and idempotently
  converges still-replayable entries (queued/dequeued/interrupted) fenced by
  an intent or by an exact attached terminal `operator_aborted` Turn to
  `Aborted`
- compatibility for records persisted before this fence matches only the
  exact attached attempt/Turn carrying `operator_aborted`, never a prompt,
  timestamp, WorkItem, or bare reason string
- unfinished dispatch memory uses a separate exclusive-startup abort proof:
  the original intent, source/Turn/run, terminal queue and settled
  attempt/outcome are budgeted and revalidated under the existing startup
  session capability, not accepted as an ordinary recoverable interruption

Reason:

- an aborted input is an explicit operator decision, not crash evidence; the
  old shape settled it as plain `Interrupted` and every recovery surface then
  treated it as replayable, so a restart could silently re-run input the
  operator had cancelled
- persisting the fence with the posture projection (before ACK) makes the
  negative decision durable even when the producer dies between ACK and its
  terminal settlement
- ordinary crash interruption keeps its existing replay recovery; only inputs
  with exact durable abort evidence lose replay eligibility
- a whole-WorkItem or Control-Stop cancellation would destroy independent
  tasks, waits, and timers; fencing the exact claimed source preserves them
