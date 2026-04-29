# OpenAI Incremental Continuation

OpenAI `previous_response_id` continuation is implemented as transport-local
request lowering, not as runtime prompt semantics.

The runtime still builds a complete, replayable provider turn request. The
OpenAI transport stores only the previous request shape, full input items,
response output items, and response id, scoped by the prompt cache agent id and
cache key. Requests without that stable scope do not reuse a continuation
snapshot. A later request uses `previous_response_id` only when its full input
is a strict append-only extension of the previous request input plus the
previous response output.

Any uncertain case falls back to a full request: prompt or cache shape changes,
missing cache scope, tool schema changes, turn-local compaction, missing
response ids, non-append conversation shape, or provider errors. Provider
errors also clear the scoped local continuation snapshot before retry so retry
attempts do not depend on an ambiguous server-side state.

Runtime events record secret-safe request-lowering diagnostics such as hit/miss
status and fallback reason, but they do not expose response ids or make
incremental continuation part of provider-neutral prompt assembly.
