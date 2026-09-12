# Runtime DB sidecar scans use `O_PATH` metadata handles

## Choice

On Linux, runtime DB sidecar FD inspection stabilizes a matched
`/proc/self/fd/<n>` entry with an `O_PATH` handle before comparing its target
and inode.

## Reason

A normal reopen, `dup`, or close of another ordinary descriptor for the same
inode can release the process's traditional POSIX record locks. SQLite manages
those descriptors with delayed close semantics, so the consistency check must
not introduce an independent ordinary open/close cycle. Directly re-reading
the numeric FD path without a stable handle would instead reintroduce the FD
reuse and ABA race fixed by #2890.

`O_PATH` pins the observed object for `readlink` and `fstat`-style metadata
without opening it for data access or participating in those record locks.
Failure to obtain that safe handle is fail closed except when the numeric FD
disappeared before stabilization.

## Preserved boundary

The check still rejects deleted-open and open/canonical inode divergence from
#2850, retains the stable-object FD reuse protection from #2890, and does not
change the process-lifetime identity cache or its cache-hit fast path from
#2892.
