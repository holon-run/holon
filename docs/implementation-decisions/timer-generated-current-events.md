# Timer producers prepare their complete Current Event set

A repeated timer with an existing pending wake writes a suppression audit;
cancelling a timer-bound Work wait may also write a new Work revision audit.
Live acceptance showed both transactions rolling back when these events were
missing from original Current address preparation.

Prepare all exact audit addresses before canonical writes. Suppression uses a
stable per-timer/fire-count ID. Cancellation derives the exact bound Work's
next revision from the original transaction selection. Keep scope qualification,
complete Current Event membership equality, atomic rollback and idempotence.
Do not remove the guard, omit the generated event, or qualify a source after
mutation. No schema migration or historical database repair is needed.
