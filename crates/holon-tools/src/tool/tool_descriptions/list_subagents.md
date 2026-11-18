List the calling agent's directly supervised children, including running,
retained and stopped children. Runtime-bound caller identity and current
canonical supervision determine visibility; creation, lineage and historical
invocations alone do not. This is not a global or recursive agent browser.

Use limit=1..100 (default 20) and the opaque cursor returned as next_cursor.
Entries are ordered by agent_id; next_cursor=null ends the list. Each page
rechecks current visibility, not a historical snapshot. Malformed, foreign or
stale cursors fail explicitly; restart without a cursor if its anchor was
removed or its supervision changed. Cursors are continuations, not authority.

Each entry has identity/incarnation, supervision state, committed lightweight
runtime status (null if unavailable), and an invocation task reference if known.
GetAgent provides individual details; TaskStatus provides task progress. This
query neither loads/wakes a child nor consumes a result or proves completion.

include_cleanup defaults to false. When true, add the first current database
blocker and observed_since when available, without changing list membership.
This is incomplete diagnostic evidence, not deletion eligibility or permission.
DeleteAgent independently rechecks incarnation, supervision, execution, files
and worktrees. Do not stop valid work merely to make cleanup eligible.
