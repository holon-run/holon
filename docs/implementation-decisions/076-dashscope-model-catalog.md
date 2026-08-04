# DashScope model catalog

Holon models Alibaba Cloud Model Studio's pay-as-you-go, Token Plan, and
Coding Plan surfaces as routes of the canonical `dashscope` provider.

The plan routes use the exact model allowlists published by Alibaba Cloud.
Models available from the pay-as-you-go endpoint are not automatically added
to either plan, and a model appearing in one plan is not evidence that it is
available from the other.

Intrinsic reasoning and image-input capabilities follow Alibaba Cloud's model
and client configuration documentation. In particular, the Qwen3.5 and
Qwen3-Coder models, GLM 5 and 4.7, and Kimi K2.5 support reasoning. Kimi K2.6
supports image input on the documented Token Plan route. MiniMax-M3 is kept as
a 192k text-input model; capabilities published by MiniMax for its direct API
are not projected onto Alibaba Cloud's deployment.

Qwen3.8-Max (GA, 2026-08) is recorded with reasoning and image input per
Alibaba Cloud's model listing and Qwen platform documentation, with a 131k
max-output ceiling. `qwen3.8-max-preview` is retained alongside the GA entry
because it remains in the Token Plan team allowlist without an announced
retirement date. Alibaba Cloud documents `deepseek-v4-flash-0731` as a
distinct model code on the Token Plan team allowlist rather than an alias of
`deepseek-v4-flash`, so it is registered as its own route with the same
1M/384K shape; the undated `deepseek-v4-flash` entry stays because both codes
are allowlisted.

Alibaba Cloud's Anthropic-compatible API accepts explicit
`thinking.budget_tokens`, but the documentation does not define stable named
reasoning levels for this transport. Holon therefore records reasoning support
without presenting an invented `low` / `medium` / `high` selector.

The legacy Beijing endpoint remains the built-in default because Alibaba Cloud
states that it continues to work. Workspace-specific regional domains can be
configured by users and are not suitable as a static built-in URL.

Sources (checked 2026-08-04):

- Text generation model catalog:
  `https://help.aliyun.com/zh/model-studio/text-generation-model/`
- OpenCode configuration for pay-as-you-go, Token Plan, and Coding Plan:
  `https://help.aliyun.com/zh/model-studio/opencode`
- Anthropic-compatible Messages API:
  `https://help.aliyun.com/zh/model-studio/anthropic-api-messages`
