# 135 Anthropic Prompt-Cache Capabilities

Anthropic-compatible endpoints differ in how their caches are driven. The
official API honors explicit `cache_control` breakpoints; DashScope support
varies by model (see the reviewed route default below). Deepseek
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

## Reviewed route default: DashScope Token Plan GLM-5.3

As of 2026-10-23, only `dashscope@token-plan/glm-5.3` defaults to
`cache_control=false`. The [official GLM capability table](https://help.aliyun.com/zh/model-studio/glm)
lists GLM-5.3 as implicit-cache only, while GLM-5.1 supports explicit and implicit
caching. The [Token Plan quick start](https://help.aliyun.com/zh/model-studio/token-plan-personal-quick-start)
documents a dedicated Anthropic-compatible endpoint but does not promise
explicit cache breakpoints. This is a conservative route default, not evidence
that this endpoint rejects or ignores markers. Other models and endpoints keep
their defaults.

The existing precedence remains route override → provider override → route
built-in → endpoint built-in → compatibility default. An operator can explicitly
re-enable markers through `models.routes` or a provider override; no persisted
user configuration is rewritten. The request gating described above also
removes Claude-Code billing/metadata/temperature mimicry, but preserves the
system/context, conversation and tools.

Response cache-usage parsing is independent of outgoing markers. Offline
provider-path tests use a permissive local mock to compare default/true/false/true
requests under both strategies; their returned cache counts are fixtures, not
upstream measurements. An earlier real Token Plan call reported cache reads
without a marker-on/off comparison. Neither proves marker support or predicts
cache hit rate, cost or latency after this default change.
