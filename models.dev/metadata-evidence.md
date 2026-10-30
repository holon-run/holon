# Capacity review: 2026-10-25

This record supports the initial route metadata coverage baseline. Existing
unchanged catalog facts are preserved, not newly certified as vendor hard
limits. An upstream candidate is evidence to review, not permission to change
an endpoint, region, subscription entitlement, or runtime output policy.

## Incremental review: 2026-10-31

The frozen models.dev payload is
`490b395ef18abff5b3fadfeffef6604ffbc79e1523d111eb0710335c63bc59e3`.
The existing `mistral-large-4` alias changes only its reasoning enum:
`none`/`high` becomes `low`/`medium`/`high`. The official
[Large 4 family page](https://docs.mistral.ai/models/mistral-large-4-0) and
[reasoning reference](https://docs.mistral.ai/capabilities/reasoning/)
agree on the latter values. Preserve all previously reviewed capacity,
modality and output-policy fields; this is not renewed certification of
those unchanged fields.

The new `mistral-large-4-0-8` candidate is not yet admitted. The public
family page does not expose that exact wire ID and gives only abbreviated
1M / 256k capacities, insufficient to independently confirm the candidate's
1,048,576 / 262,144 integers. Retain the raw snapshot/artifact record and
defer both newly drafted and locally retained unreviewed instances through
`endpoint_review_required`. The existing alias is not removed or replaced.
The previously deferred [DeepSeek candidate](https://docs.mistral.ai/models/deepseek-v4-flash-1017)
now explicitly lists text/image input, superseding the earlier text-only
evidence; its unresolved exact capacities and effort enum still prevent
admission. Neither deferral is an unsupported/retirement claim.

Five changed OpenRouter audit records agree with its independently sampled
[public listing](https://openrouter.ai/api/v1/models): output ceilings are
147,456 for DeepSeek V3.2, 81,920 for Qwen3.6 27B, 131,072 for Qwen3.8
2.4T A95B and GLM-5.3-Flash, and 235,929 for Qwen3.8 27B. Context fields
also match. These artifact projections do not overwrite static routes or
establish output defaults. NEAR AI's three additions, Venice's removed
Sonnet ID and changed MiniMax description do not establish native endpoint
eligibility or retirement.

The [Go Models API](https://opencode.ai/inference/go/v1/models) now publishes
complete native records, not merely ID availability. Of 28 public IDs,
27 match registered native protocols and base URLs. These confirm 22
existing-route differences, not 22 new routes:

| Native endpoint / IDs | Newly confirmed fields |
| --- | --- |
| Chat `kimi-k3.1-pro` | context 1,048,576; output ceiling 131,072; image input; reasoning |
| Chat `iceman-contributor-free` | context 262,144; output ceiling 131,072; reasoning; still text-only |
| Messages `minimax-m3` | output ceiling 131,072, not MiniMax's ordinary API ceiling |
| Chat `deepseek-v4.1-flash`, `glm-5.3`, `glm-5.3-flash`, `kimi-k3.1-pro` | effort `low/high/max` |
| Chat `deepseek-v4-pro`, `glm-5.2` | effort `high/max` |
| Chat `hy3` | effort `none/low/high` |
| Chat `hy4-preview` | effort `none/high` |
| Chat `kimi-k3` | effort `max` |
| Chat `minimax-m3.1-flash`, `beam-free` | effort `low/medium/high/xhigh/max` |
| Chat `step-5-preview` | effort `low/medium/high` |
| Messages `claude-haiku-5-5` | effort `low/medium/high/xhigh/max` |
| Messages `qwen3.8-max`, `qwen3.8-flash` | effort `low/medium/xhigh` |
| Responses `grok-4.7` | effort `low/medium/high/xhigh` |
| Responses `muse-spark-1.2-contributor`, `muse-spark-1.3-contributor` | effort `minimal/low/medium/high/xhigh` |
| Responses `gpt-5.6-luna`, `gpt-6-luna` | effort `none/low/medium/high/xhigh/max` |

Preserve all existing output defaults/runtime policies, preferred routes,
unmentioned fields and historical IDs. New ceilings are not omitted-request
defaults. Apply discovered fields only when `api` names the model's
registered native protocol and exact base URL. The two translated protocols
must retain unknown capacity and conservative capabilities. Gemini's native
`generate-content` record does not establish native metadata for any of its
registered translated routes. Missing/mismatched records stay conservative;
cache billing fields do not establish explicit request controls.

Step router's backend and 64K prose, and BytePlus's three resolved Dola
wire IDs, are described in the [current endpoint review](endpoint-review.md#2026-10-31-incremental-review).
No guessed Step integer or client-config-only BytePlus service limit is
certified. These changes supersede earlier unresolved-ID/all-models-listed
and 250K descriptions without rewriting the historical samples.

## Incremental review: 2026-10-30

The [endpoint review](endpoint-review.md#2026-10-30-incremental-review)
records six affirmative Qianfan retirements and one exact ordinary Volcengine
capacity change. The [K3.1 card](https://ark.volcengine.com/models/kimi-k3-1-pro.md)
specifies a 1,048,576 output ceiling; the
[ordinary model list](https://ark.volcengine.com/region:cn-beijing/docs/model-list.md)
now agrees on the maximum, resolving the earlier conflict. Set only that
route's ceiling, not its default, max input, runtime output policy or the
independent subscription limits. The 4k versus 131,072 default conflict
remains unresolved.

The [Qianfan retirement table](https://cloud.baidu.com/doc/qianfan/s/zmh4stou3)
names the six removed IDs and applies to the registered V2 Chat service.
Do not infer retirement of the unlisted experimental ERNIE ID or other
providers' identically named DeepSeek offerings. Existing unmodified facts
remain preserved, not newly certified. Native DeepSeek image support and
Codex metadata already match the current evidence; Mistral's single pending
candidate still lacks complete endpoint-specific exact metadata.

The second endpoint group also rechecked Fireworks' model-specific
[Qwen3.8 effort controls](https://docs.fireworks.ai/api-reference/post-chatcompletions.md).
Both models and the Qwen router are raw-only candidates, not static routes.
Confirmed model controls do not establish router binding, precise capacity
or general serverless eligibility. Go's conflicting listing/documentation
and BytePlus's display-name versus exact wire-ID evidence remain unresolved;
neither changes static capacity fields or proves a retirement.

## Incremental review: 2026-10-29

### Native DeepSeek compatibility coverage (#3788)

Independently reviewed the native Messages and Responses endpoints using the
[Anthropic compatibility reference](https://api-docs.deepseek.com/guides/anthropic_api/),
[Responses reference](https://api-docs.deepseek.com/guides/responses_api/), and
[pricing/model table](https://api-docs.deepseek.com/quick_start/pricing/).
Both endpoints support the current `deepseek-flash` ID and the retained
`deepseek-v4-flash` / `deepseek-v4-pro` IDs. The frozen native models.dev node
provides 1,048,576 context and 393,216 output candidates; the existing reviewed
1,000,000 / 384,000 runtime ceilings remain conservative and unchanged.
These differences remain visible rather than being silently copied or waived.

The unpinned `deepseek@default/deepseek-flash` compatibility route is now
enumerated explicitly by a reviewed association, while Responses remains
preferred. Its capacity and capability sources remain model-level; no pinned
route or new runtime behavior is manufactured for this audit. The current
Messages reference explicitly supports image content (base64, URL, and file),
so this review does not assert that Messages is text-only. Reasoning effort
options still resolve separately for Messages and Responses. Native evidence
does not establish Token Plan or reseller entitlement, limits, or capabilities.
No paid inference was run.

The new Mistral `deepseek-v4-1-flash-1017` candidate remains in the frozen
snapshot/artifact, not the admitted supplement. Its
[native model page](https://docs.mistral.ai/models/deepseek-v4-flash-1017)
identifies text and function calling, but abbreviated 1M/128k figures do not
establish exact 1048576/128000 integers or the upstream
`none`/`low`/`high`/`max` effort values. Defer this single candidate with
`endpoint_review_required`; preserve all 55 previously admitted supplement
models. This is incomplete metadata review, not evidence of unavailability.

The [endpoint review](endpoint-review.md#2026-10-29-incremental-review)
records affirmative, endpoint-scoped retirement evidence, not a family-wide
ban. Volcengine's Coding and Agent Plan exact `kimi-k2.7-code` IDs ended on
2026-10-28 at 14:00 UTC+8; ordinary API and other providers remain independent.
Retained supplement routes must not resurrect either retired subscription ID.
Existing `kimi-latest` forwarding is uncertain, not proven retired.

Fireworks' public `accounts/fireworks/models/kimi-k2p6`, `kimi-k2p7-code`
and `glm-5p2` IDs ended serverless service on 2026-09-25, while on-demand
deployments remain supported. Public catalog retirement is not permission
to delete user deployment identities, inherit those deployments' limits,
or retire native Moonshot/GLM and other providers' routes.
Unknown/contradictory deployment availability and M3 image evidence stay
unresolved rather than being converted into model-family capability claims.

Current Go discovery lists 30 IDs; eight historical IDs are absent but not
affirmatively retired. Preserve previously reviewed three-protocol access
and unknown translated-route fields. BytePlus's 13 historical plan routes
are retained without claiming all thirteen are newly certified by current
public documentation. Codex, DeepSeek and other reviewed endpoint fields
have no newly confirmed capacity change; runtime defaults remain distinct.

## Incremental review: 2026-10-28

The previous Codex admission gap is now resolved by endpoint-specific evidence:
OpenAI's [bundled Codex catalog at the reviewed revision](https://github.com/openai/codex/blob/126a0ee92ed2c5e73cd8a8f7e37563e8527552d7/codex-rs/models-manager/models.json)
adds `gpt-6.1-terra`. Its default context is **500000**, not the general API's
**1050000** or the opt-in maximum **872000**. Output default and ceiling are
not specified; retain unknown capacities and the independent 8192-token runtime
fallback. The metadata explicitly lists text/image input, parallel tools,
shell execution, low verbosity, and `low/medium/high/xhigh/max/ultra`.
It does not establish image generation or account-specific availability.
Register this reviewed Codex model explicitly; do not relax the supplement's
requirement for independent admission of future API-to-Codex candidates.

OpenCode's [Go catalog](https://opencode.ai/v2/docs/console/go/) and
[harness integration guide](https://opencode.ai/v2/docs/console/harnesses/)
now distinguish native API formats from translated access. All 30 current
Go text IDs have access to the existing Chat, Messages, and Responses endpoints,
including the exact `gemini-4-argon` ID. This text eligibility is not evidence
that translated routes inherit native vision, reasoning controls, cache
controls, or token capacities. Preserve existing native-route metadata and
preferences; narrow newly translated routes conservatively. Keep historical
names independently registered rather than renaming or expanding them from
their absence in the current table. The harness guide requires a stable
`x-opencode-session`; this catalog review does not verify live session setup.

The [Volcengine Responses effort matrix](https://ark.volcengine.com/region:cn-beijing/docs/responses-api-deep-thinking.md)
now describes K3.1 `none/minimal` as disabling thinking while also saying
thinking is always enabled. The [model card](https://ark.volcengine.com/models/kimi-k3-1-pro.md)
still specifies always-on thinking and `low/high/max`; the
[Responses schema](https://ark.volcengine.com/region:cn-beijing/docs/create-model-responses-api.md)
adds conflicting sampling descriptions. Do not add disabling effort values
or resolve the existing output-capacity conflict by inference.

The independently reviewed endpoint sets and future EOS cutoffs are recorded
in [the endpoint audit](endpoint-review.md). No paid inference, credentials,
account qualification, or deployment was used as evidence.

## Incremental review: 2026-10-27

The official [GPT-6.1 Terra API model page](https://developers.openai.com/api/docs/models/gpt-6.1-terra)
confirms a 1,050,000-token context window, a 128,000-token output ceiling,
text/image input, and `none`/`low`/`medium`/`high`/`xhigh`/`max` reasoning.
Tool calling requires Responses; Holon's built-in `openai@default` already
uses that transport. Its supplement keeps the existing 95% prompt policy
(997,500 tokens) and conservative parallel/image-generation/interactive flags.
The output default is an explicit runtime policy, not a separate capacity claim.

The public Codex model metadata checked this cycle does not list
`gpt-6.1-terra`. That absence is not retirement evidence, but it is insufficient
to admit a new ChatGPT-sign-in route. New OpenAI-to-Codex supplement aliases
therefore require endpoint review; previously reviewed aliases retain their
sticky admission, subject to the existing affirmative retirement/modality filters.
At the Terra-only checkpoint, the API route was the sole field-review addition
and all 600 previous projections were unchanged. The separate Volcengine
repairs below require their own review; that checkpoint is not their approval.
No account-specific availability or paid inference was tested.

### Volcengine native Responses qualifications and EOS

The [ordinary API retirement notice](https://ark.volcengine.com/region:cn-beijing/docs/model-deprecation-notice.md)
has model-specific exceptions: DeepSeek V3.2 `251201` reached EOS on
July 30, 2026, not the batch's September 21 date; Seed 2.0 Lite/Mini `260215`
reached EOS on October 22, not the batch's November 24 date. These ordinary
API notices do not remove independently registered coding/plan routes.
Supplement models always imply a default API route, so their ordinary
retirement filter must reject new and retained copies without deleting
subscription-only built-in registrations.

The [model matrix](https://ark.volcengine.com/region:cn-beijing/docs/model-list.md)
and [Responses effort matrix](https://ark.volcengine.com/region:cn-beijing/docs/responses-api-deep-thinking.md)
confirm Seed 2.1 Pro/Lite `260915` and DeepSeek 4.1 Flash
`261017`/`260910`/Economy `261017`, with image input and reasoning.
`minimal/low/medium/high` and `none/low/high/max`, respectively, are actual
effort levels; compatibility aliases do not add levels. Native `k` shorthand
does not certify exact context, server output default, or output ceiling.
Unknown values continue to use independent internal runtime-budget fallback.

The [K3.1 native card](https://ark.volcengine.com/models/kimi-k3-1-pro.md)
confirms the direct ID `kimi-k3-1-pro`, Responses, image input,
`low/high/max`, and exact context 1,048,576. Its output default/ceiling
conflict with the general matrix, so both remain unknown. The dotted plan ID
and the dated internal response ID are not additional direct API aliases.
Implicit caching and native Responses cache controls do not establish
Anthropic explicit `cache_control`.

Other endpoint rechecks, source restrictions, future retirement dates, and
unresolved exact capacities are retained in [the endpoint audit](endpoint-review.md).
The second independent group found no new code-level delta across its 14
registered endpoints; this does not newly certify previously unknown limits.

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

## Incremental review: 2026-10-26

The [Fireworks MiniMax M3 card](https://fireworks.ai/models/fireworks/minimax-m3)
states that generation can use the full context window, reducing `max_tokens`
when the prompt would exceed it. The same serverless model in the
[official model index](https://fireworks.ai/models) has
`slug=fireworks/minimax-m3` and integer `contextLength="512000"`.
Together these support a reviewed advisory output ceiling of **512000** for
`fireworks@default/accounts/fireworks/models/minimax-m3`, not a universal
deployment guarantee. Preserve the absent Holon default and runtime fallback
**8192**; neither the capacity nor the FAQ's service default **2048** becomes
the Holon generation budget.

The [Tencent Kimi Chat comparison](https://cloud.tencent.com/document/product/1823/132232)
independently publishes **131072** default / **1048576** maximum output for
`kimi-k3`, `kimi-k3.1-pro`, and `kimi-k3.1-pro-highspeed`.
It gives **32768** default output for `kimi-k2.8-preview` and the two
`kimi-k2.7-code*` models; retain the latter's existing **262144** ceiling.
These are exact Chat-route fields, not Messages/Responses capacities.
No exact context integer or contradictory default effort is accepted.
The three K3 Chat routes require `max_completion_tokens`, not `max_tokens`;
the adapter selects this field by exact provider/endpoint/model identity for both
request construction and effective-budget clipping. Other Chat routes retain
`max_tokens`. Local wire tests cover default, explicit small and context-clipped
budgets; no paid request or observed service-side alias rejection is claimed.
Automatic preferred models/routes also honor endpoint capability narrowing;
the TokenHub Responses default remains the previously reviewed `hy3`.

The [Qianfan model table](https://cloud.baidu.com/doc/qianfan/s/rmh4stp0j)
and [v2 Chat reference](https://cloud.baidu.com/doc/qianfan-api/s/3m7of64lb)
support five additional exact DeepSeek IDs with output ceiling **393216**,
two ERNIE thinking IDs and four Qwen 3.5 IDs with **65536**, and four GLM IDs
with **131072**. Their default output and abbreviated context remain unknown.
The table contradicts the old **248832** context for ERNIE 5.0/5.1/thinking
preview and **121856** for X1.1: it reports **128k** and **64k**, respectively.
Clear those unsupported integers instead of selecting a `k` conversion or
substituting maximum-input lengths. Missing raw context uses the existing
internal fallback; that fallback is not a newly verified vendor capacity.
No Baidu subscription endpoint or historical model retirement is inferred.

## Remaining original builtin candidates

The other 18 candidates have exact snapshot numbers but no completed
same-endpoint output-capacity review in this batch:

- Chutes: `unsloth/Mistral-Nemo-Instruct-2407-TEE`; verify the TEE deployment.
- Fireworks: `accounts/fireworks/models/{gpt-oss-120b,nemotron-3-ultra-nvfp4}`;
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
