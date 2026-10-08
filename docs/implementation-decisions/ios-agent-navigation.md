# Agent-context navigation on iOS

Use one typed SwiftUI navigation stack: Agent home → conversation → that Agent's
Work/tasks/files. Global settings are a toolbar destination, not a bottom tab.
The stack's first Agent route owns selection; pushing a child or dismissing an
editor does not select a different Agent or restart its streams. A native pop
from conversation returns directly to the list. One small confirmed-Agent
bookmark can restore only after authentication and authoritative roster membership
are revalidated in the exact API/network/runtime/user/visibility partition.

The roster remains the membership/status authority. Publish it before fetching
operator previews, enrich only appearing rows (at most 80 missing reads per
viewport update and two concurrently), and fence responses to the snapshot generation. Search
uses the complete body-bounded roster; list pagination is a rendering budget,
not silent membership truncation. Offline roster caching remains disposable and
bounded separately from live membership.

Inline local messages come from the transactional outbox, not invented turns.
A returned message ID joins canonical input IDs; matching canonical inputs replace
local rows. Receipt means accepted, not completed. Unknown/failed retry remains
explicit and reuses the immutable request UUID. The composer still edits only
the selected Agent's durable draft; late system picker results retain their
original scope/generation. Sharing remains independently confirmed.
