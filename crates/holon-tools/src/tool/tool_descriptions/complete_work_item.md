Mark an owned open work item completed by ID. After this tool succeeds, the runtime requires a non-empty operator-facing completion report and promotes that text as the canonical result.

Prefer finishing required queries, verification, and material collection before calling this tool. When no continuation tools are needed, provide the completion report as assistant text in the same round as the CompleteWorkItem call. A deferred `awaiting_completion_report` result has not committed completion yet: finish any remaining authorized cleanup or verification with the available ordinary tools, then provide a non-empty final report as text. There is no report-specific tool-round budget or read-only restriction; general loop budgets, cancellation, and normal permissions still apply. Do not call `CompleteWorkItem`, `WaitFor`, `PickWorkItem`, or `Sleep` while a report is pending. Changes to the original target revision or execution binding invalidate that pending completion.

If the target is the WorkItem bound to this execution, completion settles the execution, may resume its yielded direct caller, and ends the turn. If the target is different, completion is detached: it completes only that target and preserves the current execution, run, focus, and turn.

An active WorkItem cannot be completed from another execution or through control HTTP. Let its owning execution complete it, or stop that execution before retrying. Detached completion that would resume a caller while another execution is active is also rejected rather than replacing the active execution.

Do not call this tool for a WorkItem until that target's objective and verification are complete.

After a plan is approved, typically update the same WorkItem's plan_status from needs_input to ready, update the todo_list, and continue implementation in the same turn — do not call CompleteWorkItem just to transition from planning to implementation.

If you must split the objective into a separate WorkItem, create and activate the successor first so it enters durable runnable state, then complete the old WorkItem. Never complete the current WorkItem before a successor or continuation is established, or the remaining work will not be resumed.

After bound completion, do not claim that unfinished work will continue automatically unless a durable runnable successor or caller continuation was actually established. After detached completion, continue the current execution objective normally.
