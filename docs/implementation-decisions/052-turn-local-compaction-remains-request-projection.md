# Turn-Local Compaction Remains Request Projection

Decision:

- keep turn-local compaction as an in-memory provider request projection, not a
  durable replacement history checkpoint

Reason:

- `maybe_compact_agent` owns durable cross-turn message compaction
- OpenAI remote compaction owns opaque provider-window replacement state
- turn-local compaction only keeps one runtime turn below the provider prompt
  budget by projecting older completed rounds into deterministic recaps

This preserves Holon's readable runtime state as the semantic source of truth
while avoiding a third durable compaction format inside the same fix.
