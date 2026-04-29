# Fix for Issue #289: execution_environment Missing Multi-Workspace Information

## Problem Description

The `execution_environment` section in the agent prompt only showed a single workspace's ID and anchor path, even when multiple workspaces were attached. This made it difficult for agents to understand which workspaces were available and their locations.

## Root Cause

1. The `ExecutionSnapshot` struct only stored a single workspace's information (`workspace_id` and `workspace_anchor`)
2. The `execution_policy_summary_lines` function only rendered this single workspace
3. The `AgentState` stored `attached_workspaces: Vec<String>` (workspace IDs), but there was no corresponding storage of workspace paths for all attached workspaces

## Solution

### Changes Made

1. **Modified `ExecutionSnapshot` struct** (`src/system/types.rs`)
   - Added `attached_workspaces: Vec<(String, PathBuf)>` field to store all attached workspaces with their IDs and paths
   - Updated `ExecutionSnapshotSerde` helper struct to include this field
   - Updated deserialization logic to populate this field
   - Updated `EffectiveExecution::snapshot()` to initialize with empty vec

2. **Modified `execution_policy_summary_lines` function** (`src/system/host_local_policy.rs`)
   - Changed to display all attached workspaces when multiple are present
   - Format: `Workspace: <id> @ <path>` for each workspace
   - Falls back to single workspace display if `attached_workspaces` is empty

3. **Updated `execution_snapshot_for` function** (`src/context.rs`)
   - Populates `attached_workspaces` with the current active workspace (temporary solution)
   - TODO: Load all attached workspace paths from storage

4. **Updated all `ExecutionSnapshot` construction sites**
   - Added `attached_workspaces: vec![]` initialization in:
     - `src/runtime.rs`
     - `src/tui.rs`
     - `src/prompt/mod.rs`
     - `src/tui/projection.rs`
     - `src/runtime/provider_turn.rs`

## Testing

- Verified compilation passes with `cargo build --release`
- No test failures introduced

## Future Improvements

The current implementation only populates `attached_workspaces` with the active workspace. To fully support multi-workspace display:

1. Modify `execution_snapshot_for` to accept `&AppStorage` parameter
2. Query `storage.latest_workspace_entries()` to get all workspace entries
3. Filter to `agent.attached_workspaces` and collect (id, path) pairs
4. Pass this information through the call chain where `ExecutionSnapshot` is created

## Related Issues

- GitHub Issue #289: execution_environment 缺少多 workspace 信息
