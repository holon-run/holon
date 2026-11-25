Request durable deletion of an ephemeral child currently supervised by you.
Supply `agent_id` and `incarnation` from `GetAgent` identity or
`ListSubagents`. Caller identity
comes from your runtime and cannot be supplied. You may delete a retained child
once it is no longer useful.

Before requesting deletion, read the child's current identity and incarnation
together with any reported cleanup blocker; do not guess or reuse an identity
from an earlier observation. Delete only a retained child you currently
supervise and have confirmed is no longer needed; the end of one invocation or
task does not by itself make a child deletable. Background cleanup reminders are
ordinary internal input and require no special reply format or renewal action.

Accepted requests return a deletion job, not proof that physical cleanup has
finished. Repeating the request for the same incarnation returns its job.
Active execution (running or queued work, active waits, pending settlements),
descendant responsibility, protected artifacts, and unsafe worktrees block
cleanup. Open WorkItems and timers do not: the parent decides whether they are
still needed, and the cleanup job cancels admitted timers. A terminal command
task's output reference and runtime-owned `tool-artifacts/`, `task-output/`,
and `work-items/` files do not block cleanup. User deliverables (for example
nonempty `work/`, `notes/`, `media/`, or `apps/`) still block this safe policy.
The tool and operator CLI share the deletion core and path/worktree safety
checks, but the CLI's explicit deletion policy may discard user home data or
cancel accepted work while this tool's safe policy may not. The separate
`delete_on_terminal` child lifecycle also uses the destructive deletion policy.
A blocker is information about what still protects the child: resolve the cause
first, and do not cancel valid work, stop live tasks, or remove a child still in
use just to make cleanup succeed. Public, independent, peer, and recreated
identities are excluded. No automatic identity TTL applies.
