List up to 16 ephemeral children currently supervised by the calling agent.
Each entry contains its agent ID, incarnation, first database blocker, and the
time the current evidence was first observed when available. Use TaskStatus
for the progress of a known task. When a retained child is no longer needed,
use its ID and incarnation with DeleteAgent. Deletion rechecks eligibility and
may report additional blockers.
