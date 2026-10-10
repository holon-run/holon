Request durable deletion of an ephemeral child currently supervised by you.
Supply `agent_id` and `incarnation` from `GetAgent.identity`. Caller identity
comes from your runtime and cannot be supplied. No reminder or special reply is
required. You may delete a retained child once it is no longer useful.

Accepted requests return a deletion job, not proof that physical cleanup has
finished. Repeating the request for the same incarnation returns its job.
Active execution (running or queued work, active waits, pending settlements),
descendant responsibility, protected artifacts, and unsafe worktrees block
cleanup. Open WorkItems, timers, and retained command-task outputs do not: the
parent decides whether they are still needed. Resolve the reported blocker
first; do not cancel valid work merely to make cleanup eligible. Public,
independent, peer, and recreated identities are excluded. No automatic identity
TTL applies.
