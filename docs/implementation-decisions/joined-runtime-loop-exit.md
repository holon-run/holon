# Acknowledging the original runtime loop exit

A forced Host loop exit can skip its normal transient run-handle cleanup.
Retained runtime references then keep that stale handle, and the strict durable
readback guard correctly refuses a different canonical run posture.

Bind the original loop monitor to the physical runtime instance at construction.
The Host's actual registered monitor must finish, execution admission must be
closed, and the instance registry UUID, incarnation and owner epoch must match
before clearing its local run handle. Keep a monotonic per-instance physical
exit observation so late original ACK installation cannot restore the handle.

Canonical State, Turn and Task settlement retain their original writers and
acknowledgements. Registered command/OS producer exit remains a separate proof;
worktree cleanup still needs the complete original fleet proof. The live-run
durable-readback guard stays strict. Foreign or unfinished monitors cannot mint
this acknowledgement, and a replacement runtime gets its own registration.
