---
title: Runtime status enum inventory
summary: Machine-readable baseline for stable serialized runtime lifecycle and status enums.
order: 27
---

# Runtime status enum inventory

Holon's stable runtime lifecycle labels are generated from typed Rust enums and
checked in as
[`runtime-status-enum-inventory.json`](./runtime-status-enum-inventory.json).

- **Primary source:** the named Rust enum definitions and their serde rename
  attributes.
- **Generated inventory:**
  `holon::contract_inventory::runtime_status_enum_inventory()`.
- **Drift check:** `make snapshots-check`.
- **Refresh:** `make snapshots-refresh`, followed by review of the generated
  JSON diff.

## Current coverage

The first baseline covers:

- `AgentStatus`
- `WorkItemState`
- `WorkItemPlanStatus`
- `WorkItemReadiness`
- `TaskStatus`
- `WaitConditionStatus`
- `TimerStatus`
- `QueueEntryStatus`
- `ToolResultStatus`

The checked values are the serialized snake_case contract, not the Rust
variant spelling. Adding or removing a variant, or changing its serialized
name, fails the snapshot check until the change is intentionally refreshed.

This inventory is deliberately narrow. It does not classify every internal
enum as stable, parse Rust source text, or turn prose-only states into a
machine contract.
