# Endpoint-specific catalog review

Reviewed for the **2026-10-16 Beijing time** maintenance cycle. This is an
evidence record, not a claim that every account, region, subscription tier, or
model was exercised. No credentials were read and no paid inference was called.

The general API catalog is not a subscription allowlist. Preserve previously
reviewed routes unless there is affirmative removal evidence. New routes require
their own endpoint evidence; missing capability flags are conservative omissions,
not proof that a model lacks the feature.

## 2026-10-17 incremental review

- Rechecked the official [OpenCode Go protocol table](https://opencode.ai/v2/docs/console/go/)
  and its [public model list](https://opencode.ai/inference/go/v1/models).
  Add the exact Chat IDs `step-5-preview` and `beam-free`; do not alias them to
  `step-5-preview-free` or `cat-jam-free`, or infer retirement of those older IDs.
  Their advisory limits and capability flags come from the refreshed
  **Go-specific** models.dev records, not the older IDs or a vendor API:
  Step 5 Preview has context/output `1000000/65536` with image input; Beam Free
  has `1048576/131072` with text input. Both advertise reasoning and tool calls.
  These are secondary catalog specifications, not independently tested service
  hard limits. Beam Free is free for a limited time and has regional exclusions.
- Rechecked the [Codex model lifecycle](https://learn.chatgpt.com/docs/models).
  The recommended `gpt-6.1-sol`/`gpt-6-luna` entries and six retired
  ChatGPT-sign-in IDs are already covered. No further Codex change was confirmed;
  model selection still does not grant account access.
- Refreshed models.dev revision
  `f11359f4b3df31af3e62211d3585e8eec01c3c9dc88d9997d413621159335eeb`.
  Aggregator audit data adds four OpenRouter and three Venice records and changes
  five OpenRouter output limits; it does not auto-draft aggregator runtime routes.
  The [OpenRouter public API](https://openrouter.ai/api/v1/models), rechecked at
  2026-10-17 02:17 UTC, agrees with all nine changed/added OpenRouter records'
  context/output values. The earlier DeepSeek output-limit discrepancy belonged
  to an intermediate refresh, not this revision's diff; no runtime override is
  installed.

### Other special endpoints rechecked

| Endpoint | Current decision and remaining evidence boundary |
| --- | --- |
| `tencent-tokenhub@default`, `@messages` | The [official protocol matrix](https://cloud.tencent.com/document/product/1823/130079) confirms Chat and Messages for 21 previously Chat-only or missing IDs, including `deepseek-v4.1-flash` and `step-5-preview`. Add their missing routes without inferring integer limits or capabilities. The matrix explicitly excludes Messages for `hy-mt2-pro`, `hy-mt2-plus`, and `hy-mt2-lite`; retain only their Chat routes. The two new IDs are absent from the [general model table](https://cloud.tencent.com/document/product/1823/130078), a source discrepancy, not an account-availability test. `glm-5v-turbo` is upcoming retirement, not retired. |
| `deepseek@responses` | The [Responses guide](https://api-docs.deepseek.com/guides/responses_api/) explicitly documents `deepseek-flash` and image input through Responses. Register that exact ID on Responses only, with model-level image input; leave unpublished integer limits unset rather than inheriting V4 specifications. `deepseek-v4-flash` and `deepseek-v4-pro` remain non-vision models. The manifest uses an exact allowlist; its shared image ceiling permits Flash vision but does not assert vision for every model, and its limit ceilings do not supply missing model limits. Built-in `web_search` remains exclusive to `deepseek-v4-pro`, respecting configured opt-out; Flash ignores it. Stateless restrictions remain. |
| `dashscope@coding-plan`, `@token-plan` | Existing supplement routes for `kimi-k3`, `qwen3.7-flash`, and `qwen3.8-omni-flash` remain **unconfirmed on each endpoint** (six routes), not newly verified additions. Preserve them pending affirmative removal evidence; the [Coding list](https://help.aliyun.com/zh/model-studio/coding-plan) does not establish their eligibility. Add the independently documented Token Plan `auto` router conservatively; do not inherit its underlying models' capabilities or integer limits. Retain the [personal](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview)/[team](https://help.aliyun.com/zh/model-studio/token-plan-team-overview) eligibility union. `decision-model-preview` requires a [dedicated systemone endpoint](https://help.aliyun.com/zh/model-studio/token-plan-decision-model), not an ordinary chat route. |
| `xiaomi@token-plan` | Reconfirmed existing V2.6 Pro/Flash exact limits `1048576/131072` and image support from the [OpenCode example](https://mimo.mi.com/docs/en-US/tokenplan/integration/opencode), and the [Codex Responses base](https://mimo.mi.com/docs/en-US/tokenplan/integration/codex-configuration). Preserve both V2.5 IDs until the [announced stop](https://mimo.mi.com/docs/en-US/updates/deprecate) on **October 21, 2026, 10:00 Beijing time**. They currently redirect to V2.6; the announcement disagrees internally on the October 14 redirect start hour, not the final stop time. No additional Ultraspeed subscription eligibility was established. |
| `stepfun@plan` | The [plan integration](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api.md) independently confirms `step-router-v1`. Register it as a text-only router, without inheriting underlying vision or reasoning-effort controls. The [Chat](https://platform.stepfun.com/docs/zh/api-reference/chat/chat-completion-create.md)/[Messages](https://platform.stepfun.com/docs/zh/api-reference/chat/messages-create.md) restrictions prohibit image/document input and `web_search`, cap output at published `250K`, and ignore `output_config.effort`. Exact integer context/output limits remain unset rather than guessing the meaning of `K`. Existing four plan models remain. |
| `byteplus@coding` | [Quick start](https://docs.byteplus.com/en/docs/modelark/coding-plan-personal-get-started) confirms the existing 13 IDs; no retirement was established. Enable vision for `dola-seed-2.0-code` and `bytedance-seed-code`, explicitly marked “Multimodal visual understanding” in the [overview](https://docs.byteplus.com/en/docs/modelark/coding-plan-personal-plan-overview) model table. This fixes a conservative baseline omission, not an October 17 release. Keep this review distinct from Volcengine. |
| `volcengine@coding` | [Quick start](https://docs.volcengine.com/docs/ark/coding-plan-personal-get-started?lang=zh) (updated October 13)/[overview](https://docs.volcengine.com/docs/ark/coding-plan-personal-plan-overview?lang=zh) confirm the existing model names and the `glm-latest` alias for GLM-5.3. Add the alias only to Coding with unknown integer limits; do not infer Agent Plan eligibility. `kimi-k2.7-code` is still upcoming retirement, not removed. Kimi K3 requires thinking; Pro is recommended for Coding, not a hard tier restriction. GLM-5.3 publishes `1024k/128k`; existing integer values are conservative defaults, not newly verified service limits. |
| `volcengine@plan` | **Current verification incomplete:** the [overview](https://docs.volcengine.com/docs/ark/agent-plan-personal-plan-overview?lang=zh) and [quick start](https://www.volcengine.com/docs/82379/2373738) returned error shells, not current model-list bodies. Preserve the October 16 evidence and seven unconfirmed dated routes below. Do not claim “unchanged” or substitute Coding eligibility. |

## 2026-10-16 decisions and unresolved evidence

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
