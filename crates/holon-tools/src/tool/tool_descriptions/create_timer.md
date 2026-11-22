Create a persistent timer for the current agent. Use this for independent or repeating scheduling, not ordinary turn delays or fallback rechecks. The timer does not pause the current turn; call WaitFor with wake=timer and resource=<timer_id> when the current work must wait for it.

Use `instruction` for the trigger message and optional `title` for display only. Legacy `summary` still supplies the instruction when used instead of `instruction`. If neither name is provided, the scheduler uses its default timer message.
