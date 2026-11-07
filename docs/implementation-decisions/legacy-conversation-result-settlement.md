# Legacy conversation result settlement

The v0.30.0 operator dispatcher persisted its final result/failure brief before
persisting the terminal turn and its `produced_brief_ids`. Ordinary replies did
not write a `delivery_summaries` row. Conversation schema 64 originally seeded
`result_settled` only from delivery summaries or an explicit no-brief reason,
leaving those completed legacy results permanently unsettled after upgrade.

When schema 64 creates a missing revision, also accept a terminal turn's sealed
brief linkage: its indexed terminal kind must match the payload, and a produced
brief ID must resolve to a canonical result/failure brief for the same agent and
turn. Active turns, ACK-only turns, absent/mismatched briefs, and terminal turns
without final-result evidence remain unsettled.

This is migration-time reconstruction, not a read-time terminal heuristic.
Existing revision rows remain authoritative (`INSERT OR IGNORE`); native turns
still require the normal settlement write. This does not replay input ACKs,
change input-consumption fences, or add work to production history reads.

The repair applies to upgrades that have not yet run schema 64, including
v0.30.0/schema 25. It does not rewrite an already-migrated database's settlement
bits; repairing such databases would require separately scoped migration
provenance rather than guessing from current terminal/brief state.
