# Endpoint-specific catalog review

Reviewed for the **2026-10-16 Beijing time** maintenance cycle. This is an
evidence record, not a claim that every account, region, subscription tier, or
model was exercised. No credentials were read and no paid inference was called.

The general API catalog is not a subscription allowlist. Preserve previously
reviewed routes unless there is affirmative removal evidence. New routes require
their own endpoint evidence; missing capability flags are conservative omissions,
not proof that a model lacks the feature.

## Decisions and unresolved evidence

| Endpoint | Review result | Boundary / unresolved evidence |
| --- | --- | --- |
| `openai-codex` | Remove six retired ChatGPT-sign-in IDs from static definitions and supplement projection: `gpt-5.2`, `gpt-5.3-codex`, `gpt-5.3-codex-spark`, `gpt-5.4`, `gpt-5.4-mini`, `gpt-5.5`. | API-key models remain. Current GPT-6 models already exist; catalog eligibility does not establish account access. [Official model lifecycle](https://learn.chatgpt.com/docs/models). |
| `dashscope@token-plan` | Add `glm-5.3`, `deepseek-v4.1-flash`, `deepseek-v4-pro-0813`; retain the union of personal/team eligibility. | Limits/tools for these additions come from the endpoint-specific `alibaba-token-plan-cn` models.dev snapshot, **not** confirmed official hard limits. [Personal](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview), [team](https://help.aliyun.com/zh/model-studio/token-plan-team-overview). |
| `dashscope@coding-plan` | No automatic additions from either the API or Token Plan lists. | `kimi-k3`, `qwen3.7-flash`, `qwen3.8-omni-flash` are not established as eligible. Absence is not retirement. [Coding Plan](https://help.aliyun.com/zh/model-studio/coding-plan). |
| `xiaomi@token-plan` | V2.6 Flash/Pro routes already exist in the baseline. Remove the unsupported baseline V2.6 Pro Ultraspeed subscription route, preserving its standard API model. | V2.6 Pro Ultraspeed lacks subscription-route evidence: the supported-model list omits it, and the Desktop FAQ says current Token Plan tiers do not include it. That FAQ concerns Desktop integration, not observed rejection by every Token Plan API endpoint; do not auto-expand routes or infer model retirement. V2.5 Pro Ultraspeed eligibility is merely unconfirmed. Recheck the announced V2.5 transition on October 21, 2026, 10:00 Beijing time. [Integration](https://mimo.mi.com/docs/en-US/tokenplan/integration/opencode), [FAQ](https://mimo.mi.com/docs/en-US/quick-start/faq/token-plan/desktop-guide). |
| `stepfun@plan` | Step 5 already exists; no duplicate addition. | Router is deferred: its API restrictions must not inherit vision/reasoning from its underlying models. [Plan integration](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api.md), [Router API](https://platform.stepfun.com/docs/zh/api-reference/chat/messages-create.md). |
| `byteplus@coding` | Add only the independently documented BytePlus Coding IDs, not Volcengine names or dated IDs. | Coding base is `/api/coding/v3`; `/api/v3` incurs separate charges. Retain unknown exact integer limits as unset. V4 Flash/Pro remain preview in this offering. [Quick start](https://docs.byteplus.com/en/docs/ModelArk/1928261), [overview](https://docs.byteplus.com/en/docs/ModelArk/1925114). |
| `volcengine@coding` | Add nine missing subscription IDs; preserve existing IDs, including `ark-code-latest`. `Auto` is a console choice, not a model ID. | Use Coding evidence independently of Agent Plan. Do not switch to paid `/api/v3`. [Quick start](https://docs.volcengine.com/docs/ark/coding-plan-personal-get-started?lang=zh), [overview](https://docs.volcengine.com/docs/ark/coding-plan-personal-plan-overview?lang=zh). |
| `volcengine@plan` | Independently confirm the same nine additions in the Agent Plan table. | Kimi K3 requires Medium/Large/Max, not Small. `kimi-k2.7-code` is **upcoming** retirement, not removed. Plan keys are distinct from Coding keys. Keep existing OpenAI Responses transport; this review did not establish a transport regression. [Overview](https://docs.volcengine.com/docs/ark/agent-plan-personal-plan-overview?lang=zh), [quick start](https://www.volcengine.com/docs/82379/2373738). |
| `opencode-go` default / `messages` / `responses` | Follow the current per-model protocol table; add Responses rather than send those IDs through Chat. Use published `/inference/go/openai/v1` and `/inference/go/anthropic/v1` bases. | Existing IDs missing from the new table are not automatically deleted. New model limits/capabilities may use the **Go-specific** models.dev snapshot as secondary evidence, not vendor API metadata. Google-only Gemini and entries without sufficient metadata are deferred. Clients must preserve a stable session ID and identify their application via User-Agent. [Protocol table](https://opencode.ai/v2/docs/console/go/), [public IDs](https://opencode.ai/inference/go/v1/models). |
| `tencent-tokenhub` default / `messages` | Add 19 newly confirmed language-model IDs to Chat only. Retain existing DeepSeek IDs, including Tencent's dated and slash-qualified IDs. | The general table is not a per-model Messages matrix. New exact limits/capabilities remain unset; do not infer native vendor limits or structured-output support. `/v1/models` requires a key and was not called. [API/model table](https://cloud.tencent.com/document/product/1823/130078), [Messages guide](https://cloud.tencent.com/document/product/1823/135874). |
| `deepseek@responses` | Remove incorrect built-in web-search advertisement; keep the existing stateless transport contract. | Officially supports function tools and custom `apply_patch`, ignores built-in tools and `parallel_tool_calls`. `deepseek-flash` image examples do not establish new image support for every existing versioned ID. [Responses guide](https://api-docs.deepseek.com/guides/responses_api/). |

## Volcengine subscription additions

Both lists independently establish these IDs:
`doubao-seed-evolving`, `doubao-seed-2.1-pro`, `doubao-seed-2.1-lite`,
`doubao-seed-2.0-mini`, `minimax-m3`, `glm-5.3-flash`,
`deepseek-v4.1-flash`, `kimi-k2.8-preview`, `kimi-k3`.
This does not add their default API routes.

### Historical Agent Plan routes: support unconfirmed

The supplemental read-only review found exact subscription-name evidence for
`doubao-seed-evolving` in all four tiers. It did not establish that these
seven existing dated API IDs are accepted as Agent Plan model names:
`deepseek-v4-flash-ga-260731`, `deepseek-v4-pro-ga-260813`,
`doubao-seed-2-1-pro-260628`, `doubao-seed-2-1-turbo-260628`,
`doubao-seed-character-260628`, `glm-5-2-260617`, and
`glm-5-3-flash-260828`.

The matrix uses subscription aliases such as `deepseek-v4-flash`,
`deepseek-v4-pro`, `doubao-seed-2.1-pro`, and `glm-5.3-flash`, not those
dated API IDs. Preserve the historical routes pending exact-ID evidence;
absence from the matrix is not proof of rejection or retirement. The
quick start requires matrix model names, an Agent Plan key, and `/api/plan`.
Do not substitute default API IDs or a Coding key. See the Agent Plan
overview and quick-start sources in the endpoint table above.

The published Agent Plan context/output values are retained here in their
original units rather than guessing whether `k` means 1000 or 1024:

| Model | Context / output |
| --- | --- |
| Seed 2.0 Mini | 256k / 128k |
| Seed 2.1 Pro, Seed 2.1 Lite, Seed Evolving | 1024k / 256k |
| MiniMax M3, GLM-5.3, GLM-5.3 Flash, Kimi K3 | 1024k / 128k |
| DeepSeek V4 Flash, V4 Pro, V4.1 Flash | 1024k / 384k |
| Kimi K2.8 Preview | 1024k / 1024k |
| Kimi K2.7 Code (upcoming retirement) | 256k / 32k |

New records leave exact integer limits unset. Existing conservative limits are
not relabeled as newly verified hard limits. The Coding overview independently
confirms vision for MiniMax M3, Kimi K2.8 Preview/K3, GLM-5.3 Flash and DeepSeek
V4.1 Flash; it also confirms Kimi K3 thinking cannot be disabled.

## OpenCode Go discovery boundary

The public model list is `/inference/go/v1/models`, independently of the
`/inference/go/openai/v1` inference base. An unauthenticated GET returned 37
model IDs; appending `/models` to the inference base returned HTTP 404.
Discovery-only models retain their decoded endpoint during runtime route
enumeration, resolution, and vision candidate selection. Explicitly reviewed
built-in routes still take precedence over discovered metadata.

## Other evidence conflicts

Z.AI's GLM-5.2/5.3 model documentation says output `128K`, while the current
OpenRouter model endpoint reports substantially larger values for several IDs.
Keep the aggregator audit data and disclose the conflict; do not install a
speculative runtime override or claim an inference validation was performed.
