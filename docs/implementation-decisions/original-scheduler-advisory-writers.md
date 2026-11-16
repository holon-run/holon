# Scheduler advisories use the original Audit writer

Deferred activation, invalid interrupted replay and queue-head starvation are
scheduler observations with their own original Audit transactions. Their
metadata builders return pending events and perform no writes. Publish them
through the owner so Current event membership and
qualified notifications advance together.

A starvation advisory is selected while the scheduler already holds an owner.
Reuse that owner; beginning another mutation would deadlock. The other paths
release their planning guard before using the runtime's independent Audit
writer. Preserve existing duplicate and per-head rate limits.

This producer fix does not qualify historical rows for live Current or weaken
Current checks. Ordinary historical browsing reads committed rows without
granting execution or publication authority.
