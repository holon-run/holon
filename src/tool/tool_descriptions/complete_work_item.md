Mark an owned open work item completed by ID. Write the operator-facing completion report as assistant text in the same round; the runtime promotes that text after this tool succeeds.

If the target is the WorkItem bound to this execution, completion settles the execution, may resume its yielded direct caller, and ends the turn. If the target is different, completion is detached: it completes only that target and preserves the current execution, run, focus, and turn.

An active WorkItem cannot be completed from another execution or through control HTTP. Let its owning execution complete it, or stop that execution before retrying. Detached completion that would resume a caller while another execution is active is also rejected rather than replacing the active execution.

Do not call this tool for a WorkItem until that target's objective and verification are complete.

After a plan is approved, typically update the same WorkItem's plan_status from needs_input to ready, update the todo_list, and continue implementation in the same turn — do not call CompleteWorkItem just to transition from planning to implementation.

If you must split the objective into a separate WorkItem, create and activate the successor first so it enters durable runnable state, then complete the old WorkItem. Never complete the current WorkItem before a successor or continuation is established, or the remaining work will not be resumed.

After bound completion, do not claim that unfinished work will continue automatically unless a durable runnable successor or caller continuation was actually established. After detached completion, continue the current execution objective normally.
