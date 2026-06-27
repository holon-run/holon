# Workspace migration deprecation plan

## Context

Two migration functions exist to handle legacy workspace state from before
deterministic workspace ID generation was introduced:

1. `canonicalize_agent_home_bindings` (`src/runtime/workspace.rs`) – rewrites
   agent state that still references the old constant `AGENT_HOME_WORKSPACE_ID`
   instead of the canonical agent-specific ID.

2. `migrate_workspace_id` (`src/host_registry.rs` → `src/storage/mod.rs`) –
   lazily rewrites non-deterministic (random) workspace IDs to deterministic
   IDs in the shared workspace entry table.

## Decision

Both functions are marked as deprecated migration paths via doc comments and
emit `tracing::info!` logs with the `workspace migration:` prefix each time
they execute. This gives operators a concrete signal to determine when all
agents have migrated and the code can be safely removed.

## Removal criteria

Remove both migration functions after **3 minor releases** with zero migration
log entries, or when the next major release occurs, whichever comes first.
At that point also remove `AGENT_HOME_WORKSPACE_ID` legacy alias resolution
and the `migrate_workspace_id` storage method.
