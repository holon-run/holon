# WaitFor decouples the waiter from the task owner

Decision: `WaitFor(work_item_id=...)` no longer asserts the task owner. The
waiter is the current execution-bound WorkItem when one exists (a conflicting
explicit work_item_id is ignored and disclosed in the receipt); without an
execution binding the explicit parameter selects the waiter as before. Any
WorkItem of the same agent may hold a task-result wait; the task's captured
owner never changes and never decides who waits or who is resumed.

Reason: binding "who waits" to "who owns the task" turned a legal same-agent
dependency (execution A waiting for a task owned by B) into a validation
error and retry loop (#3124). Pausing and owning are separate concerns: the
wake layer matches task waits by task identity inside the agent and routes
the dependency wake to the recorded waiter, while the task owner keeps its
own continuation path (exact task rejoin).

Preserved boundaries: cross-agent tasks are still rejected; genuine stale
execution bindings, WorkItem revision conflicts, and the atomic
wait/brief/terminal publication contract are unchanged. One result message
still admits only one consumed trigger marker per agent (`wait_conditions`
UNIQUE(agent_id, trigger_message_id)): when several waiters share one
dependency, the task owner's own wait wins first and then the earliest wait
deterministically; the remaining waiters keep their unsatisfied waits as
durable evidence. Satisfying every waiter from one result message requires
the deferred persistent notification protocol.
