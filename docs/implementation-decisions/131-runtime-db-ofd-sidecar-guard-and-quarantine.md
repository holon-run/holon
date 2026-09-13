# Runtime DB uses a Linux OFD sidecar guard and fail-stop quarantine

## Choice

After startup migrations complete, the Linux runtime DB writer opens the
canonical database with a dedicated descriptor and holds an OFD read lock over
SQLite's shared-lock byte range for the writer lifetime.

The runtime DB protection state is published as `starting`, `protected`,
`unsupported`, or `quarantined`. A sidecar consistency failure after activation
permanently moves the process to `quarantined`: new runtime DB connections and
write transactions fail with `runtime_db_quarantined`, runtime readiness is
degraded, and HTTP error responses use `503 Service Unavailable` with a bounded
`Retry-After`.

## Reason

SQLite's Unix VFS uses traditional process-associated POSIX byte-range locks.
On Linux, closing any ordinary descriptor for the same file can release all of
that process's traditional locks for the file. That makes SQLite's live shared
lock too fragile to serve as the only barrier against another process taking
the main database `EXCLUSIVE` lock used by WAL close cleanup.

An OFD lock is associated with its open file description instead. Unrelated
descriptor closes do not release it, while its read lock remains compatible
with normal SQLite readers and writers. It does not hold a WAL read snapshot or
prevent normal checkpoint progress.

## Preserved boundary

The guard is acquired only after migrations and other startup work that may
need an exclusive database lock. The writer connection is dropped before the
guard field, so shutdown releases SQLite state before releasing the independent
sidecar protection.

The guard is defensive protection, not permission for unmanaged runtime DB
writes. Operators should still prefer runtime APIs and verified offline
snapshots for diagnosis.

Quarantine does not delete, rename, replace, recreate, checkpoint, or reconcile
the observed WAL/SHM files. A divergence despite the OFD guard indicates an
unknown protection or filesystem failure; the first release preserves that
evidence and requires explicit offline recovery rather than choosing a
generation automatically.

Non-Linux targets report `unsupported` and do not claim an equivalent
VFS-specific sidecar guarantee.
