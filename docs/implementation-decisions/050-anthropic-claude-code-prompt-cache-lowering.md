# 050 Anthropic Claude Code Prompt Cache Lowering

Holon uses a provider-lowering strategy named `claude_code_prompt_cache` by
default for Anthropic-compatible prompt-cache behavior. The provider-native
Messages API shape remains available as `messages_native` for endpoints that
need a conservative request shape.

Live cache probes showed that the decisive difference was request shape, not
streaming: a Claude Code-like body with stable prompt material in system blocks,
normal tools, body-level betas, stable metadata, and one rolling message-tail
marker repeatedly produced high cache reads against the same compatible
endpoint where Holon's previous message-heavy shape missed often.

When `HOLON_ANTHROPIC_CACHE_STRATEGY=claude_code_prompt_cache` is enabled, the
Anthropic transport moves the provider prompt frame's context blocks out of the
first conversation message and into cacheable system prefix blocks. The runtime
still builds the same replayable provider turn request. This keeps prompt
semantics provider-neutral while allowing the Anthropic wire shape to match the
cache behavior that the live probes validated.

The runtime default is `claude_code_prompt_cache`. Operators can temporarily opt
out with `HOLON_ANTHROPIC_CACHE_STRATEGY=messages_native` if a compatible
endpoint has request-shape issues. Legacy aliases `current`,
`claude_cli_like`, and `claude-cli-like` remain accepted for existing configs.

Cache lowering and beta injection are separate controls. For the official
Anthropic provider, if no explicit `HOLON_ANTHROPIC_BETAS` value is provided,
the default strategy uses the same betas that the successful live probes used:
`claude-code-20250219,prompt-caching-scope-2026-01-05`. Anthropic-compatible
third-party providers use the same `claude_code_prompt_cache` lowering by
default but do not auto-inject those Claude-specific betas unless the operator
sets `HOLON_ANTHROPIC_BETAS` explicitly.

The Rust `Default` implementation for `AnthropicContextManagementConfig` remains
neutral and does not imply live runtime defaults. Tests and fixtures should set
the strategy they need explicitly; environment/config resolution owns the
operator-facing default.

Diagnostics record the effective strategy, model, betas, and system/message
cache-control counts. These fields are for benchmark analysis and operator
inspection; they should not feed back into runtime scheduling or prompt
assembly.
