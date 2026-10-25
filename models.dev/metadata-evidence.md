# Capacity review: 2026-10-25

This record supports the initial route metadata coverage baseline. Existing
unchanged catalog facts are preserved, not newly certified as vendor hard
limits. An upstream candidate is evidence to review, not permission to change
an endpoint, region, subscription entitlement, or runtime output policy.

## Accepted missing-field updates

All numbers below are integer tokens. None of these changes populates
`default_max_output_tokens` from an output ceiling.

### DashScope Beijing default Messages endpoint

The [Messages allowlist](https://help.aliyun.com/zh/model-studio/anthropic-api-messages)
and exact model-card sections support the following `dashscope@default` IDs.
They do not establish limits for overseas/custom URLs or subscription endpoints.
All have context capacity **1000000**.

| Exact ID | Output ceiling | Official model card |
|---|---:|---|
| `qwen-plus-latest` | 32768 | [Qwen Plus, dynamic latest section](https://help.aliyun.com/zh/model-studio/qwen-plus) |
| `qwen-flash-2025-07-28` | 32768 | [Qwen Flash, dated snapshot](https://help.aliyun.com/zh/model-studio/qwen-flash) |
| `qwen3.5-flash-2026-02-23` | 65536 | [Qwen 3.5 Flash](https://help.aliyun.com/zh/model-studio/qwen3-5-flash) |
| `qwen3.5-plus-2026-02-15` | 65536 | [Qwen 3.5 Plus](https://help.aliyun.com/zh/model-studio/qwen3-5-plus) |
| `qwen3.5-plus-2026-04-20` | 65536 | [Qwen 3.5 Plus, separate April snapshot](https://help.aliyun.com/zh/model-studio/qwen3-5-plus) |
| `qwen3.6-flash-2026-04-16` | 65536 | [Qwen 3.6 Flash](https://help.aliyun.com/zh/model-studio/qwen3-6-flash) |
| `qwen3.6-plus-2026-04-02` | 65536 | [Qwen 3.6 Plus](https://help.aliyun.com/zh/model-studio/qwen3-6-plus) |
| `qwen3.7-flash-2026-07-15` | 131072 | [Qwen 3.7 Flash](https://help.aliyun.com/zh/model-studio/qwen3-7-flash) |

`latest` describes a mutable model, but its current published capacity is known
on the review date. Recheck that exact section after upstream alias changes.
Input-only and thinking-token limits are not total context or output ceilings.
Existing capability flags and default generation policy are unchanged.

### Beijing Token Plan

`dashscope@token-plan/deepseek-v4.1-flash-1017` receives reviewed advisory
context/output capacities **1000000 / 384000**, from the exact
`alibaba-token-plan-cn` record in [snapshot.json](snapshot.json).
The [official plan page](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview)
supports Beijing endpoint identity and exact-model eligibility, but is not an
independent confirmation of both integer hard limits.

`alibaba-token-plan` is the Singapore provider. Equal numbers in the current
snapshot do not make its region interchangeable with Beijing. The original
`00800363` snapshot already contained the dated model under the CN provider;
the international provider did not contain it at that time.
The review-only [endpoint association](endpoint-mappings.json) therefore binds
only the CN provider. `auto` remains a dynamic selector without a reviewed
uniform numeric capacity; no underlying model's limits are copied into it.

### DeepSeek Responses

`deepseek@responses/deepseek-flash` receives **1000000 / 384000**, preserving
the existing conservative reviewed ceiling in
[`deepseek_v1.json`](../src/model_catalog/models_dev/manifests/deepseek_v1.json).
No builtin default-endpoint eligibility is added.

The [native model page](https://api-docs.deepseek.com/quick_start/pricing/)
identifies Flash with the dated model and describes 1M/384K. The
[Chat API](https://api-docs.deepseek.com/api/create-chat-completion/)
explicitly defines 384K as **393216**, unlike the conservative **384000**.
The snapshot contains **1048576 / 393216**. These larger native candidates are
not silently substituted for Responses policy. Manifest validation currently
warns about excess; it is not a runtime clamp. Raising the reviewed ceiling
requires a separate exact-endpoint review. Neither the native context integer
nor Chat API defaults are inferred from a shorthand unit or another protocol.

### OpenAI output-only updates

The official [GPT-5.4](https://developers.openai.com/api/docs/models/gpt-5.4)
and [GPT-5.4 Mini](https://developers.openai.com/api/docs/models/gpt-5.4-mini)
cards support **128000** output tokens. Existing conservative context values
**272000 / 128000**, default generation policy, and route eligibility remain
unchanged; larger public context windows are not copied into local Codex policy.

## Remaining original builtin candidates

The other 19 candidates have exact snapshot numbers but no completed
same-endpoint output-capacity review in this batch:

- Chutes: `unsloth/Mistral-Nemo-Instruct-2407-TEE`; verify the TEE deployment.
- Fireworks: `accounts/fireworks/models/{gpt-oss-120b,minimax-m3,nemotron-3-ultra-nvfp4}`;
  verify each hosted variant, not native weights or another MiniMax version.
- Hugging Face: `openai/gpt-oss-120b`; an unspecified inference provider is not
  proof of a single deployment ceiling.
- NVIDIA: `moonshotai/kimi-k2.6`; the service reference did not establish the
  exact output integer.
- StepFun: `step-3.5-flash`, `step-3.5-flash-2603`, `step-3.7-flash`; verify
  dated IDs and both default/plan endpoints; 256000 is not 262144.
- Together: `MiniMaxAI/MiniMax-M2.7`, `MiniMaxAI/MiniMax-M3`,
  `Qwen/Qwen3.5-9B`, `meta-llama/Llama-3.3-70B-Instruct-Turbo`,
  `nvidia/nemotron-3-ultra-550b-a55b`, `openai/gpt-oss-120b`, `zai-org/GLM-5.2`;
  require hosted output evidence, preserve unusual snapshot integers and
  existing differing context policies.
- Volcengine: `doubao-seed-evolving`; generic PAYG evidence does not establish
  either coding or plan capacity and must not create a default route.
- xAI: `grok-4.3`, `grok-4.5`; current docs did not confirm exact output integers.

The two remaining original candidates, `xai/grok-imagine-image` and
`xai/grok-imagine-image-quality`, are image generation, not missing text-context
specifications. Snapshot context 16000/output 0 is not filled into text budgets.
Unconfirmed does not mean unsupported or retired. Review owners and recheck
conditions for all remaining missing fields belong in the route baseline.
