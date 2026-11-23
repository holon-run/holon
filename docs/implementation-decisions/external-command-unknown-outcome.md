# External command unknown outcomes are never replayed

Every `ExecCommand` and `ExecCommandBatch` crosses an external-process boundary
before its result is durable. The runtime therefore records a no-replay barrier
on the active turn before spawning the child process.

If startup recovery finds an interrupted turn carrying that barrier, it settles
the turn as interrupted/indeterminate, aborts its source queue claim, and emits
a recovery notice. The runtime may continue from a later explicit input, but it
does not replay the old turn or retry an external command whose outcome is
unknown. This covers wrappers, scripts, subprocesses, D-Bus, and service
managers without parsing command text.

The fence belongs to the replayable tool turn, not to whichever turn happens to
be active. Out-of-band HTTP/control command tasks do not mark an unrelated turn;
that ingress is not replayed as an `ExecCommand` call. Commands promoted from
`ExecCommand` remain fenced until their tool result is durable.

On startup the runtime can recover a durable `ExecCommand` or
`ExecCommandBatch` result missing from the transcript. A pre-execution record
without a durable result still leaves the external outcome unknown.
