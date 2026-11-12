# Index the qualified Turn-to-Transcript relation

Selected Turns expand to Transcript keys through current qualification and the
fixed seed membership view. Index that exact relation in all three underlying
tables: current, eligibility, and seed recovery. Each partial index includes
the owning scope, the related turn extracted from the fingerprint, and the
record ID, restricted to the transcript family.

Keep the existing selector and its current-over-seed and recovery-over-legacy
precedence. Querying canonical transcript rows instead would cross the fixed
qualification boundary. Indexing only transcript fingerprints also avoids
adding an index entry for unrelated evidence families.

Migration 107 uses the same names and definitions as the locally validated
indexes, with IF NOT EXISTS so those installations can adopt the migration
without rebuilding them. A large-history selector test checks results and a
SQLite VM-work budget rather than a wall-clock threshold.
