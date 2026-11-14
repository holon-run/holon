# Select recovery addresses from the active Queue

TaskResult recovery observes addresses twice around named domain preparation.
Its count, payload checks, namespace check and ordering must use the same reader
snapshot and retain the original terminal exclusion predicate.

Schema 110 adds a partial index for that predicate. The queries deliberately use
`queue_entries CROSS JOIN messages` so the active Queue drives the lookup even
before SQLite has statistics. The existing pending index covers a different
status set. Filtering `q.agent_id` would hide foreign Queue bindings that the
original observer rejects, so the Message namespace filter and foreign-binding
check remain separate. The driving set includes active Queues from other agents;
the Message join restricts results without weakening that check.

The runtime still admits the address footprint, validates its State and domain
revisions, and checks the final ID count and bytes. The database helper owns only
the four address queries; it grants no body qualification or mutation authority.
