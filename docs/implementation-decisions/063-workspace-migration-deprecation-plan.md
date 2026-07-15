# Workspace migration compatibility retirement

## Context

Two migration functions handled legacy workspace state from before
deterministic workspace ID generation was introduced:

1. `canonicalize_agent_home_bindings` (`src/runtime/workspace.rs`) – rewrites
   agent state that still references the old constant `AGENT_HOME_WORKSPACE_ID`
   instead of the canonical agent-specific ID.

2. `migrate_workspace_id` (`src/host_registry.rs` → `src/storage/mod.rs`) –
   lazily rewrote non-deterministic (random) workspace IDs to deterministic
   IDs in the shared workspace entry table and recorded alias mappings.

## Decision

The random workspace ID compatibility window has ended. The runtime no longer
migrates workspace IDs during registry access, stores workspace ID aliases, or
resolves old IDs through alias fallback. A database that still contains a
workspace entry whose ID does not match its deterministic ID is unsupported
and receives an explicit error instead of being modified implicitly.

`canonicalize_agent_home_bindings` remains separate because it canonicalizes
the reserved AgentHome identity rather than random project workspace IDs.

## Preserved boundary

- The deterministic workspace ID algorithm is unchanged.
- No `agent_states.payload_json` backfill is added for the retired migration.
- Databases from `v0.22` or earlier that never completed workspace ID migration
  must upgrade through a supported intermediate version or recreate runtime
  state.
