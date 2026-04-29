# Tool Calling Shape

Decision:

- use native Anthropic-style `tool_use` and `tool_result`
- do not invent a Holon-specific JSON action protocol

Reason:

- Holon's coding path follows the Claude Code runtime shape
- staying close to the provider-native protocol avoids an extra adapter layer
