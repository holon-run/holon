Mark an open work item completed. Write the operator-facing completion report as assistant text in the same round; the runtime promotes that text after this tool succeeds. If this work item has a yielded direct caller, completion resumes that caller as current and the turn closes for scheduler continuation.

This is a terminal operation: it ends the current WorkItem/goal and closes the turn for scheduler continuation. Do not call it until the entire objective and verification is complete.

After a plan is approved, typically update the same WorkItem's plan_status from needs_input to ready, update the todo_list, and continue implementation in the same turn — do not call CompleteWorkItem just to transition from planning to implementation.

If you must split the objective into a separate WorkItem, create and activate the successor first so it enters durable runnable state, then complete the old WorkItem. Never complete the current WorkItem before a successor or continuation is established, or the remaining work will not be resumed.

After completion, do not claim that unfinished work will continue automatically unless a durable runnable successor or caller continuation was actually established.
