# holon-domain

Shared runtime values and serialization contracts for Holon, including agents,
messages, Turns, tasks, WorkItems, workspace projections, audit events, and
tool results. The main crate re-exports its existing public type paths.

This crate depends on no other Holon crates. Provider clients, database
transactions, runtime admission, and budget ledgers live in the main crate.
