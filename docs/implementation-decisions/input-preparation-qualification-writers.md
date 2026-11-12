# Qualify Input and Scheduling in separate writers

An owner-held preparation loads the complete snapshot through a pooled reader.
If Input evidence needs qualification, that reader walks the selected
dependencies and returns their missing keys. The writer revalidates the original
scope, fixed eligibility, digests and Attempt relations for those keys. It
writes the needed aliases and leaves the history window and Current query to
the reader. Completion edges that require persisted aliases are resolved on a
fresh read. A plan is never published as an Input snapshot.

Scheduling qualification has a separate writer limited to the Scheduling
selector and its required proofs. The caller reloads Input, Scheduling, Episode,
queue and Current from one read transaction and validates the original owner
publication before installation. A failed final read leaves the published root
in place. Previously qualified sidecars remain scoped to their incarnation.
