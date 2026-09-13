# Lifecycle delivery recovery follows the canonical root

An agent-lifecycle delivery keeps its first activation as the stable canonical
root. When an interrupted delivery is claimed again, the scheduler derives the
new attempt's `recovery_of_attempt_id` by following the persisted execution
recovery chain from that root to its current interrupted leaf.

Bootstrap recovery uses the same rule before treating a later lifecycle attempt
as exactly replayable. Every link must describe the same message and canonical
lifecycle scenario, and every predecessor on the path must already be
`Interrupted`. A missing, branched, mismatched, or otherwise incomplete chain is
rejected instead of moving the delivery's canonical activation or weakening the
repository validation.

This keeps the delivery ledger as the authority for identity while allowing
multiple process interruptions to produce an explicit, auditable sequence of
execution attempts.
