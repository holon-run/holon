# Scheduler advisories use the original Audit writer

Deferred activation, invalid interrupted replay and queue-head starvation are
scheduler observations with their own original Audit transactions. Their
metadata builders return pending events and perform no writes. Publish them
through the owner so Current event membership, archive history proof and
qualified notifications advance together.

A starvation advisory is selected while the scheduler already holds an owner.
Reuse that owner; beginning another mutation would deadlock. The other paths
release their planning guard before using the runtime's independent Audit
writer. Preserve existing duplicate and per-head rate limits.

This producer fix does not adopt existing unacknowledged historical rows or
weaken Current and archive-read checks. Such rows require separately verified
recovery evidence.
