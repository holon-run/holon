#!/bin/bash
set -e
INPUT_FILE="tests/support/runtime_flow.rs"
OUTPUT_FILE="tests/support/runtime_workspace_worktree.rs"
TEMP_FILE="/tmp/workspace_worktree_migrated.rs"
head -60 "$OUTPUT_FILE" > "$TEMP_FILE"
cat >> "$TEMP_FILE" << 'IMPORTS'

use support::{attach_default_workspace, TestConfigBuilder};

// ============================================================================
// Runtime workspace and worktree domain test support
IMPORTS
sed -n '3108,4899p' "$INPUT_FILE" >> "$TEMP_FILE"
mv "$TEMP_FILE" "$OUTPUT_FILE"
echo "Migrated $(wc -l < "$OUTPUT_FILE") lines to $OUTPUT_FILE"
