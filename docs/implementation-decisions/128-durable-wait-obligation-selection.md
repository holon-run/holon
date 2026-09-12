# Durable wait obligations use derived queue selection

When a durable wait record is `Triggered` or `Resolved`, its matching queued
message is selected ahead of ordinary non-`Interject` backlog. The scheduler
derives this obligation from persisted wait evidence and uses the same
selection rule for planning, optimistic-concurrency revalidation, and removal
from the queue.

This does not add a public priority and does not rewrite the queued message's
declared priority. `Interject` remains strictly first, while unrelated messages
retain their normal priority and FIFO behavior. A queued `TaskRejoin` also
remains a protocol barrier until canonical admission either accepts or
terminalizes it; it is not ordinary backlog that a later obligation may bypass.
The choice keeps delivery correctness attached to the durable wait lifecycle
instead of encoding a temporary scheduler obligation into the public message
envelope.
