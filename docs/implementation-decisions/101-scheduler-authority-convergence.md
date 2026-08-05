# Scheduler authority convergence

Canonical scheduler rows may remain internally valid while drifting from the
authoritative WorkItem and wait lifecycle after an older runtime completes,
cancels, or rearms work.

Holon converges only drift that current durable facts prove:

- obsolete exact-wait queue inputs are dropped without becoming lifecycle
  nudges;
- completed or missing WorkItem owners are terminalized through one typed,
  fenced reducer command that also releases their exact lane reservation; and
- ambiguous conflicts retain their queue entry but use notification plus
  bounded retry rather than self-triggering a hot loop.

The existing `scheduler-recovery` command reports and applies the same typed
convergence command. Full database backup remains the default; explicit
`--no-backup` is an operator-approved cost tradeoff and does not weaken source
revalidation, maintenance locking, reducer invariants, or audit evidence.
