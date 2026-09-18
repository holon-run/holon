# Historical Turn Settlement Reconciliation Is Evidence-Fenced

Decision:

- terminal `TurnRecord` assembly reads messages, Briefs, tool executions, and
  wait conditions by exact `agent_id + turn_id` identity instead of recent
  evidence windows
- `holon debug runtime-db turn-settlement` is read-only by default and may
  optionally write a versioned JSON repair plan
- `--apply --plan <path>` requires the runtime maintenance lock, creates a
  verified database backup unless `--no-backup` is explicit, and revalidates
  every candidate fingerprint in one write transaction
- automatic repair is limited to a non-terminal Turn with one open,
  non-recovery execution attempt, its exact `Dequeued` source queue entry,
  exactly one successful terminal-intent tool execution, and no durable wait
  condition for that Turn
- repair interrupts the Turn, queue entry, and execution attempt while
  preserving existing Brief and tool evidence; it never re-executes a tool,
  republishes a Brief, or reconstructs a wait from old arguments or text

Reason:

- recent-window scans can omit evidence from long Turns and cannot serve as a
  repair fence
- successful `CompleteWorkItem`, `PickWorkItem`, and `WaitFor` records are not
  all terminal; the persisted `should_sleep` disposition distinguishes bound
  terminal intent from detached or already-consumed continuations
- historical state is ambiguous whenever attempts, terminal intents, or waits
  are not unique, so those cases remain `manual_review_required`
- separating online audit/plan generation from offline apply keeps the default
  workflow non-mutating and makes stale plans fail closed
