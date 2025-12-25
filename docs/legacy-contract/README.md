# Legacy Holon Contract (Historical Reference)

This directory contains the **legacy** prompt contract that is **no longer used** by the Holon prompt compiler.

## Historical Context

The `v1.md` file in this directory was the original prompt contract for Holon. It has been **superseded** by the new layered prompt architecture.

## Active Contract Location

The current active contract is located at:
- **`pkg/prompt/assets/contracts/common.md`** - The common contract used as the base layer for all prompts

## New Layered Architecture

The prompt compiler now uses a **layered assembly approach**:

1. **Common Contract** (`contracts/common.md`) - Base sandbox rules and physics
2. **Mode Contract** (optional, e.g., `modes/solve/contract.md`) - Mode-specific behavior overlay
3. **Role** (e.g., `roles/developer.md`) - Role-specific behavior
4. **Mode/Role Overlays** (optional) - Additional customizations

See `pkg/prompt/compiler.go` for details on how these layers are assembled.

## Migration Notes

- The legacy `contract/v1.md` path is **not consumed** by the current compiler
- The `manifest.yaml` still contains a `contract: v1` field for backward compatibility, but it is **intentionally not used**
- All prompt assembly now goes through `contracts/common.md` as the base layer

## Date Moved

December 25, 2025 - Relocated from `pkg/prompt/assets/contract/` to `docs/legacy-contract/` to avoid confusion about the active contract.
