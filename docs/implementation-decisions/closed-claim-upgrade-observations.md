# Preserve closed historical claims during the original upgrade

An Interrupted or reducer-settled Attempt can have no Run or actual Turn because its source was
Dropped, Aborted or Quarantined before execution. Requiring an Open claim's live
admission evidence for that historical record prevents unrelated agents from
starting.

Only the original fixed capture can record this closed observation. It requires
matching canonical Attempt headers, source identity/generation/fences, Queue
headers/body, Message headers/body and an exact binding-compatible terminal
Outcome. An assignment, current State binding, owning Work execution or recovery
successor prevents this observation. Missing or inconsistent evidence still fails.

Persist the terminal state and outcome ID in `closed_claim` in the existing Attempt proof JSON to distinguish the
historical observation. It remains usable only for the same terminal Attempt
with no Run and the same terminal outcome. It cannot reopen the claim, acquire a
late actual Turn, or merge with a proof containing an actual Turn. Ordinary
readers cannot create the observation; existing native admission proofs retain
their original semantics. No Queue, Outcome, Work or Task state is rewritten.

A later retry does not reopen its closed predecessor. Its existence alone does
not invalidate the original historical observation; an actual source assignment,
current binding, or WorkExecution ownership still follows the strict existing
qualification path. No successor body or Turn is inferred from the retry link.
