---
title: Model tool schema inventory
summary: Versioned inventory for Holon's model-facing built-in tool schemas, result envelopes, and stability labels.
order: 26
---

# Model tool schema inventory

This page defines the versioning policy for Holon's **model-facing built-in
tool surface**. The machine-readable inventory is checked in as
[`model-tool-schema-inventory.json`](./model-tool-schema-inventory.json).

- **Primary source:** `src/tool/tools/mod.rs` `builtin_tool_definitions()` and
  the typed Rust argument structs deriving `schemars::JsonSchema`.
- **Generated inventory:** `holon::tool::model_tool_schema_inventory()`.
- **Drift check:** `cargo test --test tool_schema_inventory_snapshot`.
- **Current status:** pre-1.0 baseline. Treat stable labels as the intended
  compatibility boundary for the current track, not as a final 1.0 promise.

## Inventory contents

Each built-in tool entry records:

- tool name
- capability family
- stability level
- model-facing input schema
- freeform grammar, when the tool accepts non-JSON input
- result envelope family and model rendering contract
- related HTTP or CLI surfaces when commands wrap tool or runtime APIs
- model-visible tool description

## Capability families

Each built-in tool belongs to a capability family.  The runtime defines six
families in `src/types.rs` (`ToolCapabilityFamily`):

| Family | Description | Example tools |
|--------|-------------|---------------|
| `CoreAgent` | Core agent operations (state, memory, work items, scheduling, CLI/config introspection) | MemorySearch, WaitFor, ListWorkItems, ListModelProviders |
| `LocalEnvironment` | Workspace-local operations | ExecCommand, ApplyPatch, ViewImage, UseWorkspace |
| `Web` | Public web access | WebFetch, WebSearch |
| `AgentCreation` | Agent spawning and delegation | SpawnAgent |
| `AuthorityExpanding` | Tools that can expand agent authority | (reserved; no built-in tools) |
| `ExternalTrigger` | External event ingress | CreateExternalTrigger, CancelExternalTrigger |

## Stability levels

| Level | Meaning |
|-------|---------|
| `stable` | Name, input schema, result envelope family, and documented model rendering are intended to be compatibility-preserving. |
| `experimental` | Surface is available but may change while the runtime contract is still settling. |
| `deprecated` | Surface remains for compatibility but should not be introduced into new workflows. |

## Naming policy

Holon-native built-in tool names use PascalCase with action-first names. Use
`List*` for collection reads, `Get*` for single-resource or snapshot reads, and
explicit verbs such as `Send*`, `Stop*`, `Create*`, or `Update*` for control or
mutation. Legacy aliases may remain accepted by the dispatcher during a
migration window, but the checked-in inventory advertises the canonical
model-facing name.

## Versioning policy

The top-level `version` field versions the inventory format, not every tool
schema independently.

- Increment `version` when the inventory file shape changes in a way that
  readers must handle differently.
- Do not increment `version` for ordinary tool additions, removals, input-schema
  changes, description changes, or stability-label changes; those are contract
  changes inside the same inventory format and are reviewed through the
  snapshot diff.
- Preserve the Rust definitions as the source of truth. Intentional changes
  should update Rust first, then refresh the checked-in inventory.

## Refresh workflow

```bash
cargo test --test tool_schema_inventory_snapshot refresh_tool_schema_inventory_snapshot -- --ignored
cargo test --test tool_schema_inventory_snapshot
```
