# Live WorkItem record writes belong to the runtime owner

`RuntimeHandle::update_work_item_record_expected(&WorkItemRecord, u64)` is the
explicit live record CAS boundary. The handle reserves its actual mutation owner,
checks the record's agent against that scope, and submits an original Work
command through `RuntimeMutation::commit_work_transition`.

This reuses the owner's incarnation, original budget, pre-write proof, actual
write-set finalizer, final State, and Work acknowledgement path. The existing
transaction validates record revisions and lifecycle transitions, synchronizes
the canonical execution source revision without replacing admitted attempt
fences, and applies the resulting runtime notifications after commit. Identical
record replay and stale or invalid revision errors keep their existing SQL
semantics.

Storage/repository writers remain offline interfaces, not live writer adapters.
They must not infer a live scope from `agent_id`, adopt raw history, or qualify
records after business SQL. Runtime callers that mutate a qualified live record
must use the explicit owner boundary; fresh, retired, and pruned qualification
rules are unchanged.

The unscoped repository update entry fails closed using Work's existing
`known_anywhere` membership test before business SQL. It rejects qualified,
pruned, and fixed-seed IDs while leaving genuinely unqualified legacy writes
offline. It does not manufacture an owner scope or a qualification acknowledgement.
