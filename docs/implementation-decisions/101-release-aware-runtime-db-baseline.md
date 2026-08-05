# Release-aware runtime DB baseline

## Decision

The first release after `v0.30.0` treats schema `25` as the published migration
floor. Databases at or below that floor run the released migrations through
schema `25`, then apply one atomic release baseline that:

- executes the real schema `26-30` data conversions;
- creates the final schema `45` scheduler, execution, and wait objects directly;
- records the covered schema `26-45` identities in `schema_migrations`; and
- records explicit provenance and a final schema fingerprint in
  `schema_migration_baselines`.

Databases already at schema `26-45` continue through the original compatibility
migrations.

## Reason

Release users never observed the intermediate scheduler rollout, shadow, owner,
and internal-followup table shapes. Replaying those shapes creates and rebuilds
empty tables without preserving additional release data. Development databases
may contain real facts at those checkpoints, so their append-only migration
history remains supported.

## Preserved boundary

The baseline is one transaction from schema `25`: any validation or DDL failure
leaves the database at the published floor. CI compares the baseline schema with
the compatibility-chain schema and runs a Docker upgrade case that creates data
with the real `v0.30.0` runtime before opening the same volume with the candidate.
