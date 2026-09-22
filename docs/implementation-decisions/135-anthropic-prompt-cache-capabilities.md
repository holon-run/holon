# 135 Anthropic Prompt-Cache Capabilities

Anthropic-compatible endpoints differ in how their caches are driven. The
official API and dashscope honor explicit `cache_control` breakpoints; deepseek
ignores `cache_control` entirely (Context Caching is automatic) and
bigmodel/GLM caches implicitly only. Sending the `claude_code_prompt_cache`
wire shape to an implicit-cache endpoint bought no cache benefit while its
mimicry changed behavior: forced `temperature=1.0` overrode the endpoint's
default sampling on endpoints that support the full temperature range.
(A caller-facing temperature passthrough does not exist yet; the request
simply omits `temperature`, so the endpoint default applies.)

The decision: cache mimicry is gated by a per-endpoint capability declared in
the provider registry, not by the strategy alone.

- `AnthropicCacheCapabilities.cache_control` (default `true`) records whether
  the endpoint honors explicit `cache_control` breakpoints. It rides
  `AnthropicContextManagementConfig`, so every transport decision reads one
  resolved configuration object.
- Built-in registry entries declare the profile: `deepseek` and `bigmodel` map
  to `AnthropicCompatibleImplicitCache`; every other Anthropic-compatible
  endpoint keeps today's behavior unless evidence says otherwise.
- The config file can override the declaration per provider via
  `providers.<id>.cache_capabilities.cache_control`, which also covers custom
  Anthropic-compatible providers pointing at an implicit-cache endpoint.

When the capability is `false`, the Anthropic transport skips all
Claude-Code cache mimicry for both strategies: no billing-header system block,
no `metadata.user_id`, no forced `temperature`, and no `cache_control`
breakpoints or rolling markers anywhere in the payload. The #3178 context
layout is preserved — non-TurnScoped context still folds into the system
prefix and TurnScoped context still rides the conversation tail — because that
layout benefits automatic prefix caching too.

Preserved boundary: the capability gates request fields, not the strategy.
`claude_code_prompt_cache` remains the runtime default; operators keep one
strategy switch (`HOLON_ANTHROPIC_CACHE_STRATEGY`) and the capability decides
which parts of the lowering are meaningful per endpoint. Claude-specific betas
are still only auto-injected for the official Anthropic provider.

Reserved for a follow-up (P2 in #3177): `tools_breakpoint` and
`long_cache_retention` capabilities would follow the same declaration pattern
once the transport implements tools-level breakpoints and the 1h TTL.
