# Fix for Issue #290: Brief Reporting Missing - Final Summary Not Showing in Brief

## Problem Description

After an agent completes a task, the final summary text does not appear in the brief, making it difficult for users to see the task completion results.

## Root Cause Analysis

### Brief Generation Mechanism

From `src/runtime/interactive_turn.rs:72-77`, the brief content depends entirely on `outcome.final_text`:

```rust
let brief = if outcome.terminal_kind == TurnTerminalKind::Aborted {
    brief::make_failure(&message.agent_id, message, outcome.final_text.clone())
} else {
    brief::make_result(&message.agent_id, message, outcome.final_text.clone())
};
```

### Final Text Source

From `src/runtime/turn.rs:363` and `src/runtime/turn.rs:490`:

```rust
let final_text = last_assistant_message.clone().unwrap_or_default();
```

The `last_assistant_message` is updated in `src/runtime/turn.rs:219-227`:

```rust
let aggregated_text = combine_text_history(&truncated_text_history, &text_blocks)
    .into_iter()
    .map(|text| text.trim().to_string())
    .filter(|text| !text.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
if !aggregated_text.is_empty() {
    last_assistant_message = Some(aggregated_text.clone());
}
```

### The Bug

**When the model calls a tool (like `Sleep`, `Write`, etc.) in the last content block without additional text content, `text_blocks` is empty, causing `aggregated_text` to be empty, and `last_assistant_message` is not updated.**

This happens when:
1. Agent outputs a summary text in a content block before tool calls
2. Then calls `Sleep` or other tools in the next block
3. Since the tool call block has no text, `text_blocks` is empty
4. `last_assistant_message` is not updated with the summary text
5. The final brief content is missing the summary

## Solution Implemented

### Code Changes

Modified `src/runtime/turn.rs` to preserve text history when the current round has no new text:

```rust
if !aggregated_text.is_empty() {
    last_assistant_message = Some(aggregated_text.clone());
} else if !truncated_text_history.is_empty() {
    // If current round has no text, preserve text history from previous rounds
    // This ensures that summaries before tool calls are not lost
    let history_text = truncated_text_history
        .iter()
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !history_text.is_empty() {
        last_assistant_message = Some(history_text);
    }
}
```

### How It Works

1. When the current round has text (`aggregated_text` is not empty), use it
2. When the current round has no text (only tool calls), fall back to text from previous rounds (`truncated_text_history`)
3. This ensures that summaries output before tool calls are preserved in the brief

## Testing

- Verified compilation passes with `cargo build`
- No test failures introduced

## Future Improvements

### Additional Prompt Guidance

While this code fix addresses the root cause, we can also add explicit prompt guidance in `AGENTS.md`:

```markdown
## completion

**重要**: 在调用 Sleep 之前，确保你的最终总结已经作为**最后一个文本 content block**输出，而不是在工具调用之后。
```

### Enhanced Brief Generation Strategy

Consider modifying `src/runtime/interactive_turn.rs` to use more than just `final_text`:

```rust
let brief_text = if outcome.final_text.trim().is_empty() {
    // If final_text is empty, extract information from recent tool results
    format!("Completed with tool results: {:?}", recent_tool_results)
} else {
    outcome.final_text.clone()
};
```

## Related Issues

- GitHub Issue #290: Brief 汇报缺失问题 - agent 完成任务后最终总结未显示在 brief 中
- Related to #289: Both issues improve agent's perception and expression capabilities
