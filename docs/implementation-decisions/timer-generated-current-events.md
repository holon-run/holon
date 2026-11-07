# Timer producers prepare their complete Current Event set

A repeated timer with an existing pending wake writes a suppression audit;
cancelling a timer-bound Work wait may also write a new Work revision audit.
Settling or quarantining a terminal timer Queue source also retires its wake
and writes a retirement audit.
Live acceptance showed these transactions rolling back when these events were
missing from original Current address preparation.

Prepare all exact audit addresses before canonical writes. Suppression uses a
stable per-timer/fire-count ID. Cancellation derives the exact bound Work's
next revision from the original transaction selection. Retirement uses its
original Message address and carries the actual appended audit back through
the Queue transition ACK. The producer attaches archive history proof in the
same transaction, including bootstrap normalization. Return audit notifications
in their original append order so streaming cursors cannot skip a helper
event. These history ACKs never grant live Current authority. Keep scope
qualification,
complete Current Event membership equality, atomic rollback and idempotence.
Do not remove the guard, omit the generated event, or qualify a source after
mutation. These producer fixes require no schema migration or historical
database repair; they do not adopt existing unacknowledged historical rows.
