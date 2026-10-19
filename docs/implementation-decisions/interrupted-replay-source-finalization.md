# Interrupted Replay Sources Finalize Instead of Rewriting Turn History

Decision:

- when startup recovery finds an `interrupted` execution attempt whose
  strictly matched attached turn already reached a different terminal state
  (`aborted`, `completed`, or any non-`interrupted` terminal), it finalizes
  the attempt side in one transaction: a new
  `FinalizeInterruptedExecution` domain command moves the attempt to
  `settled` without touching its recorded outcome, the source queue entry
  becomes `aborted` (or `processed` for a completed turn), the task-result
  settlement becomes `settled` with disposition
  `interrupted_replay_source_final` (or `model_delivered`), and message
  delivery advances to `failed`/`consumed`
- runtime turn terminal history is never rewritten to `interrupted`
- claim-time validation (`interrupted_attached_turn_id`) now rejects such
  pairs with the typed `InterruptedReplaySourceInvalid` error; the scheduler
  quarantines that queue head through the existing three-attempt
  no-progress path instead of failing the agent runtime loop

Reason:

- the persisted pair (interrupted attempt, terminal turn) is deterministic
  local state: replay-anchored admission can never succeed for it again, so
  before this change every later claim of the re-enqueued task result
  hard-failed the loop as a non-retryable error and starved all other
  wakes (#3607)
- rewriting the turn terminal to `interrupted` would trade an append-only
  guarantee for at-most-one extra task-result delivery, and is only safe
  when the turn has no result/failure brief and no replay successor; the
  attempt side already carries its interruption outcome, so finalizing it
  preserves all history while releasing the replay anchor
- the queue/settlement/delivery writes must share the finalization
  transaction, otherwise the task-result recovery scanner keeps
  re-enqueueing the same caller-admitted claim
- quarantine containment mirrors the #3484/#3492 blast-radius pattern:
  deterministic data rejection must stay scoped to one message instead of
  killing the loop, and the scanner does not re-enqueue quarantined heads

Preserved boundary:

- finalization runs only at startup recovery and only for the strict
  attached-turn match shared with claim validation; ambiguous attachments
  still surface as typed rejections for the scheduler to quarantine
- the original write path that aborted the turn without settling its
  activation attempt is tracked separately; startup finalization is the
  idempotent backstop, not a license for new drift
