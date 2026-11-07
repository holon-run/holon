# Parent cleanup after reusing a supervised child

A retained child can receive another `InvokeAgent` request as an
`agent_message_wait`. Its original Queue transaction both admits the child
Message and binds the parent wait Task. The complete receipt therefore owns
two scopes even though it does not create another supervised child.

Classify that exact original group before exporting or purging the child.
Require the checksum-validated Queue receipt and accepted delivery ACK, the
frozen correlation Task ID, the exact parent Task-only install ACK, canonical
private supervision, both incarnations, terminal request delivery and Task,
and the explicit typed target and recovery binding. A compatibility delivery
incarnation may be absent; the frozen group and original typed Task binding
must still prove incarnation. An explicit mismatching value or an unknown
typed target fails closed. Reject unrelated parent effects and automatic
command history.

The child purge removes the whole original receipt without rewriting it or
stripping one scope. Parent Task and durable result rows survive. This does
not authorize deleting a parent's history through a child reference or
removing shared provider credentials. Unknown shared commands remain blocked.


Initial `TaskStop` is different: its second child scope is an explicitly frozen
readonly observation. It has no child install keys, audits or notifications.
Classify the checksum-validated whole receipt against the exact original Stop
effect, both incarnations, private lineage and canonically cancelled parent
Task. Retain the complete receipt with the parent, including while the original
Stop continuation is pending. Child export and purge must exclude it; deleting
it would orphan the parent's `task_stop_effects` foreign key and recovery source.
Parent purge may own it only after the child's private cleanup proof is complete.
Unknown or nonempty child effects remain blocked. Scope membership alone does
not imply private content ownership.

Every receipt excluded by the private-content selector still enters original
ownership validation, even if malformed normalized scope membership hides its
claimed other-scope Stop owner. Retention predicates cannot become a path
around whole-receipt checksum and normalized-descriptor checks.
