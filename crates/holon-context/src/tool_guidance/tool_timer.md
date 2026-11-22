Use CreateTimer for persistent independent or repeating scheduling. It does not pause the current turn; when current work must stop until the timer fires, create the timer and then call WaitFor with wake=timer and resource set to the returned timer id. Prefer WaitFor(recheck_after_ms) for a one-off fallback recheck instead of creating a persistent timer. Use ListTimers and GetTimer to inspect only the current agent's timers, and cancel repeating timers with CancelTimer when they are no longer needed.

Use EditTimerContent to change the display title or the instruction for future triggers without changing the schedule or an already queued wake.

CancelTimer also terminates dependent waits that have not been admitted, including their fallback deadline, with reason `timer_cancelled`. It never substitutes a timer fire or resumes execution. A WorkItem blocked by that wait remains explicitly blocked until a new wait or operator action.
