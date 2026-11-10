# Resource estimates do not control runtime availability

Cumulative synchronous resource estimates are advisory. Remove the production
64MiB/65536-node phase admission and subtraction of previous estimates. Do not
replace them with larger limits or an opt-out flag. The normal constructor is
observational even when runtime history/pending sizing values are small.

The counters approximate encoded/decoded structures, copies and parser scratch;
they are not allocator usage, RSS or the peak live set. Completed intermediate
work may stay counted, so normal history growth could make an otherwise valid
Agent state read, startup or prompt fail with HTTP503. Estimated totals cannot
serve as a reliable availability boundary.

Keep scope/ledger ownership, last-holder accounting, arithmetic validity and
real I/O failure propagation. Existing explicit finite test-only observations
remain fault-injection tools; they cannot be configured or constructed by the
running product. Regressions must use the normal constructor to prove that
exceeding the former byte/node thresholds still preserves full valid results.

The follow-up continuity upgrade repair also removes independent cumulative
read/materialization quotas and pending/history admission quotas, including
ordinary requests. Legacy state-memory sizing fields and internal request hints
remain readable for compatibility and diagnostic observations; they do not
reject valid reads, pending tickets, startup, or writes. Complete dependency
queries do not silently truncate at these former quotas. Explicitly requested
pagination/window cardinality remains a selector, and streaming migration page
sizes remain traversal choices. A single valid event larger than a page target
must still advance the cursor.

Preserve source qualification, canonical hashes, execution ownership, source
generations, structural recursion checks, publication fences, accounting
ownership and arithmetic overflow checks. No larger threshold or opt-out flag
replaces the removed quotas.

Ordinary view publication and capture do not construct a full history report.
Removing cumulative admission also removes those implicit report scans and their
host-wide ledger lock. Registration and last-holder release still update the
exact allocation memberships; test audits build the complete report on demand.
Final-view release also reaps weak-only allocations incrementally so removing
implicit scans does not retain
dead allocation identities indefinitely. This cleanup quantum is a traversal
choice, never a capacity or admission limit. Reporting cost must not gate runtime
or control-plane availability.

Context snapshot assembly skips the aggregate encoded-size estimate of the
assembled inputs. The snapshot retains its selected record holders and
resource acquisition guards. Source validation, actual read failures, and
explicit test faults still run at their existing boundaries.
