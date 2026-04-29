# 045 Anthropic Context Management

Anthropic context management is provider lowering, not runtime prompt assembly.

Holon keeps constructing a full semantic continuation request. When
`HOLON_ANTHROPIC_CONTEXT_MANAGEMENT` is enabled, the Anthropic transport may add
provider-native context-editing options for older tool-use history. The runtime
does not delete model-visible conversation state before lowering.

The first policy is conservative: keep the recent tool-use tail, do not count
errors as eligible, and exclude patch and operator-delivery tools from the
eligible clearing estimate. Benchmark diagnostics report enabled rounds and
approximate eligible tool-result bytes without storing raw tool payloads in the
summary.
