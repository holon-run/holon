# Capacity review: 2026-10-25

This record supports the initial route metadata coverage baseline. Existing
unchanged catalog facts are preserved, not newly certified as vendor hard
limits. An upstream candidate is evidence to review, not permission to change
an endpoint, region, subscription entitlement, or runtime output policy.

## Incremental review: 2026-11-20

`gemini@default/gemini-4-neon`: the native `generateContent` API supports text
and image input with text output and function calling. Keep only the inputs
and capabilities already implemented in the Holon transport. The unified
context is **1,048,576**, and the default and ceiling for one direct
non-streaming REST request without continuation are **65,536**. The four
thinking levels are model metadata; the transport does not send a level
override, so Google applies its default (`high`). The prompt budget of
996,147 is Holon's 95% policy, not a vendor input limit.

- [Exact model card](https://aistudio.google.com/docs/models/gemini-4-neon),
  November 19, 2026 20:35:44 UTC, SHA256 `1ba63d467f68138528c75610a69977555f339170c4e1b2261d1f6d79b1e2ea01`.
- [Neon guide](https://aistudio.google.com/docs/whats-new-gemini-4-neon),
  20:37:00 UTC, SHA256 `42a17b8f84840f9f03b68b98a21337e5516542b6c0ef4023909add13b3728618`.
- [GenerateContent long-decode guide](https://aistudio.google.com/docs/long-decode?generate-content=true),
  20:37:01 UTC, SHA256 `cbe313080aec5003687aca40dc946a1d329f003950047ab62061bfa3feda2bb3`.

The four retired `minimax@default` routes no longer have active metadata
records. Their absence is endpoint-specific, not a change to the
capacities of MiniMax models on unrelated routes. The sampled
[international FAQ](https://platform.minimax.io/docs/faq/models.md)
has SHA256 `0183792f121a53fe2ad6bb724a4fc464b20543959175d85a707025884f37a96d`;
the [China FAQ](https://platform.minimax.cn/docs/faq/models.md)
has SHA256 `ad27a06064bd5ea1130bc38ed6c95383a363e1e9485174c96310dde2c664d3dc`.
See the [endpoint review](endpoint-review.md#2026-11-20-incremental-review)
for the date and subscription boundary.

## Incremental review: 2026-11-19

Pinned models.dev SHA256
`53fe4bbc0f0911bf07abb49956b71302d1cd269817cfac0ace5ea540bd8fad97`,
sampled **November 18, 2026 20:38:31 UTC**, against main `50a4e1e2`.
Upstream still supplies 256000 context and output for the exact
`volcengine/doubao-seed-character-260628` ID. This conflicts with the official
[model list](https://ark.volcengine.com/region:cn-beijing/docs/model-list.md):
128K context/input, 32K answer, 32K reasoning, and 4K answer default.
The list was sampled at **20:39:19 UTC**, HTTP 200, SHA256
`5ea6928ba2e4226d70279ba03ba5e97e914eb90e88e927deecbb61bea8274f4e`.
The previous sample listed 96K input and 128K reasoning; today's corrections
do not establish exact integer token counts.

The independently sampled
[Responses API contract](https://ark.volcengine.com/region:cn-beijing/docs/create-model-responses-api.md)
(20:39:19 UTC, HTTP 200, SHA256
`72a998bc7ae8178ff93a34693d3ad8bda5ebfd67abe23b275ac214816f3ac8ed`)
defines `max_output_tokens` as answer plus reasoning tokens. It establishes
131072 as the three DeepSeek Flash IDs' default, not as Seed Character's
default or a universal hard ceiling. No precise total-output default/ceiling
is established for Seed Character by the separate abbreviated answer and
reasoning maxima.

The
[plan overview](https://ark.volcengine.com/region:cn-beijing/docs/agent-plan-personal-plan-overview.md)
(20:39:19 UTC, HTTP 200, SHA256
`191734f14310b9377457b434088f73ed1d31f08fdd66cca66114f9cf510ad10b`)
does not establish independent capacity for that exact dated ID.
The shared projection withdraws the conflicting context/output values and
derived auto-compaction from both retained routes, without inferring plan
eligibility or importing another model's limits. Unknown capacity must remain
distinct from runtime fallback execution budgets.

The exact-provider/date regression includes adjacent-date and other-provider
controls, and a catalog integration regression covers both final routes.
Strict complete-object assertions passed: only the two retained Seed Character
routes change, while the other 646 complete objects and ledger records remain
unchanged. Only the matching ledger entries are reviewed as unknown; local
128000 prompt / 8192 execution-output fallback budgets are not provider limits.
Final fixed regeneration, validation, 648-route coverage, strict assertions,
224 catalog tests (one ignored), 51 domain tests, 516 provider tests,
formatting and both warning-gated all-target checks passed.
The complete lib-c run remains non-green: 804 passed, four failed, seven ignored.
Two non-IPv6 failures each passed two exact isolated reruns; the original
concurrent failures and two listener address/network errors remain recorded.
Independent seven-file production/evidence review found no confirmed findings.
Neither that review nor the initial unchanged coverage replaces full acceptance.
The [endpoint review](endpoint-review.md#2026-11-19-incremental-review)
records all other contract changes and unresolved boundaries.

## Incremental review: 2026-11-18

Pinned models.dev SHA256
`3a6ced626e1b3e47270eb82c15ed55ab967ae74948634d46e6f7b3c7a0e4c4de`,
sampled **November 17, 2026 20:35:16 UTC**, against main `6198b476`.
Public contracts are separate from account qualification and live inference.
Unchanged fields are preserved, not newly certified.

| Exact route / field | Reviewed contract and boundary | Primary evidence sampled November 17 UTC |
|---|---|---|
| `openai/gpt-6.2-sol`, `openai/gpt-6.2-astra` | Ordinary API context 1,050,000, maximum input 922,000, output ceiling 128,000; image/text input and function calling. Do not transfer these limits to Codex. Base prices have separate long-context/cache billing conditions. | [Sol](https://developers.openai.com/api/docs/models/gpt-6.2-sol), [Astra](https://developers.openai.com/api/docs/models/gpt-6.2-astra) |
| `openai-codex/gpt-6.2-sol`, `gpt-6.2-astra`, `gpt-6.1-sol` | Default contexts 500,000 / 272,000 / 500,000, respectively. 872,000 is an opt-in maximum, not the default. New IDs preserve unknown output and explicit auto-compaction limits; expose low/medium/high/xhigh/max without adopting ultra's automatic delegation. No account-access or client-version guarantee. | [Pinned official Codex inventory](https://github.com/openai/codex/blob/8011b92bc82d4cc1b6dc3f35c0cd28768c797c97/codex-rs/models-manager/models.json), blob `56e0f43cb66259e3b8d5a59d8ff12f41e1c8be06`, SHA256 `d50b6749506d97e5f334dbabcb666eb1689f423cf3e88cfabdcf3c536b873068` |
| `tencent-tokenhub@default`, `@messages`, `@responses` / `minimax-m2.7` | Retired November 17, 23:59:59 UTC+8; remove all three routes and keep the alternate `minimax-m-2-7` absent. Account-qualified M3 migration is not continued access to the old model. Do not extend this notice to another provider or plan. | [TokenHub/CloudBase notice 2511](https://cloud.tencent.com/announce/detail/2511), independently fetched 20:44:09 UTC |
| DashScope Qwen4 API / Token Plan | Formal API adds Batch support, not a new agent request protocol. Preview remains a time-limited Token Plan entitlement. Team preview no longer lists the personal page's additional night discount; keep pricing/eligibility independent. Messages output defaults and explicit cache qualification remain unknown. | [Formal API](https://help.aliyun.com/zh/model-studio/qwen4-max), [personal Token Plan](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview), [team Token Plan](https://help.aliyun.com/zh/model-studio/token-plan-team-overview) |

The eleven-prefix evidence group archived 50 successful official responses;
the fifteen-prefix group archived 77 responses (72 successful, five failed).
Failed/empty pages are not evidence of retirement or lack of support. The four
DashScope prefixes and Codex were independently sampled by the implementation
agent. All 30 tracked prefixes have a scoped decision; no group claims
all-field recertification or live account qualification.

- [Qianfan Flash model](https://cloud.baidu.com/doc/qianfan/s/6mh4stoyf)
  and [current text-generation API](https://cloud.baidu.com/doc/qianfan-api/s/wmwbl48n4)
  now establish mandatory thinking, fixed `high`, and unsupported
  `thinking_budget` / `thinking_strategy`. Omission of effort preserves the
  fixed service behavior; unsupported explicit efforts must be rejected before
  networking. The remaining admission blocker is exact-route implementation
  and construction-path verification, not missing public reasoning/tool facts.
- [MiniMax FAQ](https://platform.minimax.io/docs/faq/models.md) states
  November 18 for four ordinary API retirements without a timezone/hour.
  Its historical retired-table/date conflict is already present in the baseline.
  Do not infer a new state transition at this UTC November 17 sample or remove
  M Plan, another region's nonexistent entries, or aggregator compatibility IDs.
- [Go model inventory](https://opencode.ai/inference/go/v1/models) confirms
  Qwen4 Messages existence, tools/images, context 1,000,000, output ceiling
  262,144, and advertised effort capabilities. It does not specify an executable
  Messages JSON reasoning field; admission remains deferred. The fifteen-prefix
  group did not perform a previous-day full-object Go hash diff and makes no
  blanket unchanged-object claim.

Final pinned refresh, validation, 648-route coverage, strict complete-object
comparison, related crate/integration tests, formatting, and `-D warnings`
all-targets checks passed. The complete local root lib-c run remains
1081 passed / 4 failed / 7 ignored: two IPv6 listener failures and two channel
failures; each non-IPv6 test passed two subsequent exact reruns. Independent
review of the 13-file frozen change (61 hunks), including final generated data
and verification documents, found no confirmed finding. Subsequent
review-status-only documentation edits were checked separately before
publication; these results do not certify account access or all inherited
metadata.

## Incremental review: 2026-11-17

Pinned models.dev SHA256
`82eb03a35fac1b36ca09218a3c64126dbf8f1bc08ddd879c5f59fd1987780282`,
sampled **November 16, 2026 22:35:28 UTC**, against main `3841ea0f`.
This is the November 17 Beijing cycle. Public material was sampled separately
for all 30 special prefixes and Codex; account qualification was not exercised.
Existing unchanged fields are preserved, not newly certified.

| Exact route / field | Reviewed contract and boundary | Primary evidence sampled November 16 UTC |
|---|---|---|
| `dashscope@default/qwen4-max`; `dashscope@token-plan/qwen4-max` | Context 1,000,000; thinking output ceiling 262,144; image/tools; efforts low/high/xhigh. Messages output default unknown. Medium maps to high and runtime max to xhigh only for this exact DashScope model. | [Formal model](https://help.aliyun.com/zh/model-studio/qwen4-max), 22:37:24, SHA256 `762d7ff7fbc3f2441f284e0f60fb54c5ed9d98d46ced4d30e2ab72aa89765c79`; current Messages/deep-thinking references linked there. |
| `dashscope@token-plan/qwen4-max-preview` | Exact context unknown ("1M"); output ceiling 393,216, default unknown; low/medium/xhigh. Runtime high/max map to xhigh. Preview stays out of default/Coding endpoints; explicit Token Plan cache disabled. | [Preview model](https://help.aliyun.com/zh/model-studio/qwen4-max-preview), 22:37:25, SHA256 `68eda18f3b6fd8e6a91236d806f07082cb7d57d54e524ee2d9c647c81be1d11c`; [personal Token Plan](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview), 22:36:10, `6033bb6128d0a28765f2c9de802e7a795882474071aaadffec226aab56b21d32`. |
| Both exact Qwen4 DashScope Messages models | Send `output_config.effort`, not `thinking.budget_tokens`; omission preserves distinct service defaults. This source does not certify Go's request fields. | [Messages API](https://help.aliyun.com/zh/model-studio/anthropic-api-messages), current model-specific controls plus the two model pages above. |
| `openai-codex@default/gpt-daybreak-blue-latest`, `gpt-daybreak-red-latest` | Default contexts 272,000 / 372,000; output default/ceiling and auto-compaction unknown; parallel tools, image and ordinary low/medium/high/xhigh/max supported. Blue experimental 872,000 maximum is not the default; delegation ultra excluded. Ordinary API metadata unchanged. | [Official Codex immutable models blob](https://api.github.com/repos/openai/codex/git/blobs/489ebe3f2483348641a16160ce8491dd077eabca), 22:59:06, SHA256 `1384625c0b6b3e67d3c60da29becea2b017f2c283fc1e2d05b8edafb4698eb10`; Git blob SHA1 independently checked. |
| `volcengine@default/doubao-seed-2-1-pro-260628`, `@plan` | Clear exact context/derived auto-compaction rather than accept 1,024,000/972,800. The dated model row says 256K, not an exact integer; 1024K is another version or dotted Plan alias. No output-default recertification. | [Dated model list](https://ark.volcengine.com/region:cn-beijing/docs/model-list.md), [Agent Plan](https://ark.volcengine.com/region:cn-beijing/docs/agent-plan-personal-plan-overview.md), sampled and re-fetched at 22:56–22:57 with unchanged SHA256. |
| `opencode-go@default/hy4-flash-free` | Native Chat context 1,048,576, output ceiling 65,536; default unknown; image/tools and top-level reasoning_effort none/high. Native API/reasoning objects match existing Hy4; no Messages/Responses capability transfer. | [Go model listing](https://opencode.ai/inference/go/v1/models), 22:37:58; [Go endpoint table](https://opencode.ai/v2/docs/console/go/). |
| Go native `kimi-k3`, `kimi-k3.1-pro` | Output ceiling 1,048,576; keep defaults 131,072 / unknown and existing effort options. Translated route limits/defaults unchanged. | The same exact Go listing objects, not Zen or native Moonshot fields. |
| `opencode-go@messages/qwen4-max` (deferred) | Listing establishes context 1,000,000, ceiling 262,144, effort labels low/high/xhigh, toggle/budget labels, 5m/1h cache. It does not identify serialized thinking controls; neither enabled/budget_tokens nor output_config.effort is certified. No route added. | Go listing/table and [official Inference docs source](https://raw.githubusercontent.com/anomalyco/opencode/dev/services/site/src/docs/content/console/api/inference.mdx); supplementary official SDK inspection distinguishes the two serialization shapes but does not prove backend acceptance. |
| `tencent-tokenhub@default,messages,responses/hy4-flash` | All three eligible with independent vision evidence. Unknown context/default/ceiling. Only Chat advertises high/no_think; other protocol reasoning remains conservative, not declared impossible. | [Hy guide](https://cloud.tencent.com/document/product/1823/132252), [thinking](https://cloud.tencent.com/document/product/1823/131208), 22:38:40; independent protocol matrix and model table. |
| `qianfan@default/ernie-light-dt-1.0` | Default output 65,536, ceiling 131,072, context unknown; tools/parallel; low/high/max. Optional thinking defaults enabled; unsupported thinking_budget omitted. | [Exact model parameter table](https://cloud.baidu.com/doc/qianfan/s/6mh4stoyf), 22:47:44, SHA256 `07341a44f58804be82972b8b1b969129dd93077033f4d0986bcf1a1e9007599f`. |
| `qianfan@default/ernie-5.5-flash-preview` (deferred) | Tools/parallel and always-on reasoning confirmed; effort is fixed high, not an adjustable menu. Abbreviated context/default/ceiling do not establish exact integers. Generic unrestricted effort handling not admitted. | Same model-specific parameter table and current function-calling model lists. |
| `xiaomi@default/mimo-v2.6-flash`; `xiaomi@token-plan/mimo-v2.6-flash` | Existing Responses hosts only. Default/ceiling 131,072; context unknown; vision/tools/reasoning; no distinct effort menu. Pro's context remains advisory client configuration, but its output fields now have direct schema evidence. | [Responses contract](https://mimo.mi.com/docs/en-US/api/chat/responses), 22:38:48, SHA256 `b6caf7f3627cf019050b241e688b7655f4a227829c41b54c55cc3466f8ee6ccf`; independent model and subscription pages. |
| Xiaomi existing default/Token Plan Responses transport | `background`, `previous_response_id`, `context_management` unsupported. Use full history; disable remote compaction. Standard request already omits background. Exact canonical provider/endpoint matching, not model-prefix stripping. | Same Responses compatibility limitations; real paid backend exercise remains outside this review. |
| `nvidia@default/nvidia/nemotron-3-super-120b-a12b` | Exact hosted max_tokens default 16,384 / ceiling 32,768; reasoning_effort none/low/high. Existing context not newly certified by this schema. | [Exact hosted schema](https://docs.api.nvidia.com/nim/reference/nvidia-nemotron-3-super-120b-a12b-infer). |
| `nvidia@default/z-ai/glm-5.2`; `moonshotai/kimi-k2.6` | Hosted default/ceiling 16,384/32,768 and 16,384/65,536 respectively. No Nemotron effort inheritance. | Each model's embedded NVIDIA OpenAPI; Kimi's raw schema includes the default even when the HTML parameter view omits it. |
| NVIDIA `minimaxai/minimax-m2.7`, `minimaxai/minimax-m3` | Hosted default/ceiling 8,192/16,384 each; not manufacturer limits or manufacturer retirement scope. | Exact model-specific NVIDIA OpenAPI, sources independently sampled in this cycle. |
| `nvidia@default/minimaxai/minimax-m3.1-flash` | Hosted default/ceiling 16,384/65,536, image/tools/reasoning; total context and standard effort menu unknown. Published input-sequence length 1,000,000 does not certify an input+output total window. | Exact NVIDIA model card and OpenAPI, not a claim that this model was first released this cycle. |
| `fireworks@default/accounts/fireworks/models/minimax-m3p1-flash` | Ready, Serverless, function tools/image/reasoning; integer context 1,048,576 from the same model's index entry. Output default/ceiling and effort menu unknown. | [Exact service model](https://fireworks.ai/models/fireworks/minimax-m3p1-flash), [official model index](https://fireworks.ai/models); do not convert display "1040k"/"1M" or inherit manufacturer 1,000,000/524,288. |
| `gemini@default/gemini-3.5-live-translate-preview` (not admitted) | Live audio input and audio/transcript output are not a GenerateContent agent text route. Block both new drafting and previous-supplement retention; do not generalize this filter to aggregators. | [Exact Live model card](https://aistudio.google.com/docs/models/gemini-3.5-live-translate-preview), 22:51:11, SHA256 `578390c7351f29a7946b5116a5a9857aa7b9fd949055510771ba8b0f2eeea23b`. |

The [endpoint sweep](endpoint-review.md#special-endpoint-sweep-and-retained-boundaries)
records independently reviewed subscription, domestic/international, retirement,
and dynamic-only boundaries. In particular, MiniMax's November 18 API retirement
does not remove November 17 subscription/aggregator routes. Kimi Coding's
high-versus-max default effort contradiction remains unresolved. The preview
discount's exact expiry is recorded there, not turned into permanent pricing.

Verification and complete-object delta approval are still pending; the earlier
192/327 passing crate totals cover only the earlier Qwen/Live patch stage.

## Incremental review: 2026-11-13

Pinned models.dev payload:
`0fd23a4d9e4a865c3a785fb69375356ce6bf73692985f9e3ae04732ff2f0faf0`,
sampled November 12 at 20:49:19 UTC. Review is for the November 13 Beijing
cycle, not a claim of fresh account entitlement.

| Exact route | Reviewed facts | Primary source and UTC sample |
|---|---|---|
| `gemini@default/gemini-4-hydrogen` | Unified context 1,048,576; direct synchronous REST default/upper output 65,536; function calling, structured output and image input; thinking low/medium/high/xhigh, default high. Text output only. | [Model page](https://aistudio.google.com/docs/models/gemini-4-hydrogen), November 12 20:56:27 UTC, SHA256 `330de74c5fb4dfbc5ec53cd9297d6c52c405281c4f1ab0e6ea82d47b1a54dea2`; [migration](https://aistudio.google.com/docs/whats-new-gemini-4-hydrogen), 20:57:43 UTC, `276991da517da8d3c8a97a74a9c470805c5821441e01ee5deafd094bce07aff3`. |
| `gemini@default/gemini-4-carbon` | Correct default/upper output 65,536→32,768 for Holon's direct non-streaming REST without continuation. Context and all capabilities/efforts unchanged. Argon already uses 32,768. | [GenerateContent long decode](https://aistudio.google.com/docs/long-decode?generate-content=true), November 12 21:04:13 UTC, SHA256 `23096646574d1911f3647e91b8a77c8547ad3493f0691988189cfffd81a8eb6c`. |

The wire-specific guide explicitly says Carbon/Argon `generateContent`
defaults to 32,768 and values above 32,768 without `continuation_token`
return HTTP 400. This supersedes the November 11 decision based on the
less-specific [Carbon model page](https://aistudio.google.com/docs/models/gemini-4-carbon),
which still says 65,536 synchronous (resampled November 12 21:02:46 UTC,
SHA256 `5ce69ebb4d7f122f4dd5e0020418b3a20975897fdd7134601d63f64625327b58`).
That statement applies to other invocation modes and is not evidence that
Holon's bare REST request supports 64K.

Hydrogen's migration guide rejects `minimal`, defaults to `high`, supports
implicit but not explicit caching, and tells clients to omit custom
temperature/top-p/top-k. Holon's existing request shape omits these fields,
does not select thinking effort, and does not implement continuation.
Advertising available effort choices is not a claim that this transport
currently sends a selected thinking level. Audio/video/PDF support in the
vendor model is not added to Holon's text/image transport by this change.

The 30-prefix special-endpoint review used fresh November 12 UTC samples:
group one archived 48 source responses and 158 files; group two archived
57 responses and 253 files. The parent checked both complete SHA256 manifests.
Their full-object comparisons use the original 635→636 refresh inputs,
not the later Carbon correction. All 332 + 142 group objects match those
initial inputs; `route_ref` is split at its first slash without dropping keys.

Source report SHA256 values:

- First group: `77b0b98039ce616918cd3f40b48ddad8a2f331b0ac97d5049893d7228f0f9af9`.
- Second group: `80d3bfbc2fcb5e92d4a8e4c809ac7983b3c7bdd0d7efcb2977803e78e04a38db`.

The [endpoint review](endpoint-review.md#special-endpoint-review-and-retained-boundaries)
records changed public contracts and unresolved eligibility separately from
static equality. Qwen4 Messages has a required output field, not an unknown
omitted default. Kimi Coding public model information now exists but default
effort sources conflict. Moonshot's future account-scoped retirement notice
is not permission to delete all current routes.

## Incremental review: 2026-11-12

Pinned models.dev payload:
`5a4aee967f0a0586ca3bd638129ff4e39bb71518f48e0ad8385fe38212426aac`,
sampled November 11 at 21:03:40 UTC. The snapshot bytes are fixed for refresh,
validation and comparison; changing upstream metadata is not permission to
extend subscription or Codex access.

The official [Create model response contract](https://ark.volcengine.com/region:cn-beijing/docs/create-model-responses-api.md),
sampled November 11 at 21:06:24 UTC, explicitly gives `max_output_tokens`
default **131072 (128K)** for `deepseek-v4-1-flash-261017`,
`deepseek-v4-1-flash-260910`, and `deepseek-v4-1-flash-economy-261017`.
The archived response SHA256 is
`c6ed8ef3fd592be440bda379a7cd8812c95e93356916c26daffadb4a51019fcc`.
The explicit integer establishes a default only. Their exact context and
output hard upper limits remain unknown, and no subscription route inherits it.
The output budget includes both reasoning and answer tokens. K3.1's independent
output conflict is not resolved by this evidence.

Alibaba's [personal Token Plan](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview)
and [team Token Plan](https://help.aliyun.com/zh/model-studio/token-plan-team-overview)
list `qwen4-max-preview` and callable alias `qwen4.0-max-preview`. The
[Messages API contract](https://help.aliyun.com/zh/model-studio/anthropic-api-messages)
explicitly limits this new model to Token Plan and documents default thinking,
effort low/medium/xhigh (high/max map to xhigh), and strict structured output.
The preview window ends November 25, 2026 at 24:00 UTC+8, equivalent to
November 26 at 00:00 UTC+8.

The subsequent [Qwen4 model card](https://help.aliyun.com/zh/model-studio/qwen4-max-preview),
sampled November 11 at 21:32:29 UTC, explicitly confirms Function Calling,
text/image/video input and output hard limit **393216**, including reasoning.
Its archived SHA256 is
`49f27b8d94a4f15d1a8eb8006ed7a81d947f21cdb5846d917d0abc956de611f4`.
The separate [Chat API contract](https://help.aliyun.com/zh/model-studio/qwen-api-via-openai-chat-completions),
sampled at 21:35:21 UTC, says the non-GLM default equals the model's maximum
output, supporting a Chat default of 393,216. Its archived SHA256 is
`f327747bd215da4c9b98a0071bf6c01e9c9adc4a60044e33b46f42ca1645f71a`.
These sources supersede the earlier tool/output evidence gap, not the deferral:
context remains only `1M Token`, and Messages/Responses defaults, runtime
request compatibility, cache details and real account eligibility remain open.
Do not invent an exact context integer or transfer the Chat default across
protocols. No runtime route or request adaptation is admitted here.

The [endpoint review](endpoint-review.md#2026-11-12-incremental-review)
records accepted and deferred changes separately. The 635-route pre-correction
complete-object comparison, including 474 objects across 30 special endpoints,
does not close historical official-field or dynamic-account evidence gaps.

## Incremental review: 2026-11-11

The Carbon-specific 65,536 projection decision below is superseded by the
November 13 direct non-streaming REST evidence above.

Pinned models.dev payload:
`f312cd2d4640b2876c8ac0b1f1878a6727f893ff38fd2a56b09fc8cb086dd433`,
sampled November 10 at 21:05:47 UTC. The
[endpoint review](endpoint-review.md#2026-11-11-incremental-review) records
the independently checked route boundary and unresolved evidence.

The native [Carbon model card](https://aistudio.google.com/docs/models/gemini-4-carbon)
and [migration guide](https://aistudio.google.com/docs/whats-new-gemini-4-carbon)
confirm the exact `gemini-4-carbon` ID, 1,048,576-token unified context,
65,536-token synchronous output, image input, text output, function calling,
structured outputs, and always-on thinking with low/medium/high levels
(default medium). Neither image generation nor explicit context caching is
supported. Long decode beyond 65,536 needs a separate continuation/background
protocol; it is not enabled by this update. Native Google projection corrects
the snapshot's stale 32,768 output value only for this exact model. Argon,
preview suffixes, resellers, and Go do not inherit the correction.

The model and migration page SHA-256 values are respectively:

- `41d56d4c27341f265c1fa822c2981fbfa8da3e53545aea5c698a3c322c1f0b5c`
- `2e036561544f22805bb165cfa0a9efe494f40f064aed53750277415c3f7d313e`

The [public OpenRouter model list](https://openrouter.ai/api/v1/models),
sampled November 10 at 22:09:29 UTC, has SHA-256
`16452f62718c997cc42c9c52ee0f6b25977416a55ef410a93213c0a01ca247e0`.
It corroborates the new OpenRouter discovery IDs and changed output limits.
Its overall `context_length` agrees with the snapshot for Qwen3.8-27B
(1,048,575), but its `top_provider.context_length` is only 262,144; this remains
an aggregator/provider distinction, not a certified limit for every backend.
The discovery-only removal of `stealth/crouton-alpha` and OpenCode's newly
deprecated Crouton metadata do not prove a checked-in route retired.

The freshly sampled [active Codex catalog](https://github.com/openai/codex/blob/126a0ee92ed2c5e73cd8a8f7e37563e8527552d7/codex-rs/models-manager/models.json)
is unchanged from the previous cycle (SHA-256
`1384625c0b6b3e67d3c60da29becea2b017f2c283fc1e2d05b8edafb4698eb10`).
The retired `core/models.json` location is not the active source.

## Incremental review: 2026-11-10

Pinned payload:
`b93c6e319c6ccec47273ecbee54934fbdaf1e1863407fd19e7cd42a833e23496`,
sampled November 9 at 21:00:28 UTC. The
[endpoint review](endpoint-review.md#2026-11-10-incremental-review) separates
accepted increments from preserved unknowns and incomplete source evidence.

The native [Fable 5.2 overview](https://platform.claude.com/docs/en/models/fable-5-2/overview)
confirms the exact `claude-fable-5-2` API ID, 1M/128K limits, always-on
adaptive thinking, unsupported forced tool choice, and omitted sampling
parameters. It also explicitly requires 30-day retention, with ZDR access
only when separately authorized. The
[effort contract](https://platform.claude.com/docs/en/build-with-claude/effort)
confirms five levels and default `high`. The sampled model overview,
thinking and effort bodies have SHA-256 respectively:

- `a6b6ea43f997802dc8b34aeaaaade72592cb872b8acb692f7a15240c06a3d85f`
- `71025f38bd32896e07b04226ea0116a55b6cb4618e03c8bbc95be3e9e39c6b5c`
- `87c3900847cb21bd1b44f466831fd97a9d19f689e65400da4157b6302a4ee55f`

The exact LongCat ID is independently listed in
[Go](https://opencode.ai/inference/go/v1/models) and
[Zen](https://opencode.ai/inference/openai/v1/models). Both objects identify
native `chat-completions`, but their base URLs differ:
`https://opencode.ai/inference/go/openai/v1` versus
`https://opencode.ai/inference/openai/v1`. Root `id` supplies the wire model;
the API object has no `id` override. `canonical_id=meituan/longcat-2.5`
is not the request ID. Published fields are text/image input, text output,
tools/reasoning true, toggle-only reasoning, context 1,000,000, output
262,144, and `reasoning_content` compatibility. The free Go offer is
time-limited. Neither the ceiling nor a reasoning toggle establishes a
service output default or effort enumeration. Messages/Responses remain
Holon translations with explicit conservative route overrides.

The [ordinary Ark list](https://ark.volcengine.com/region:cn-beijing/docs/model-list.md)
and [K3.1 card](https://ark.volcengine.com/models/kimi-k3-1-pro.md) already
disagreed on 4k versus 131,072 output default. Re-reading only the card
does not resolve that conflict; keep the existing unknown and 8,192 runtime
fallback. No ordinary-API default is projected into Coding/Agent Plan.

Codex's catalog is byte-identical to the preceding sample. DeepSeek pricing
and Responses text hashes are unchanged:
`b5491191b386267f90608fbb1d1667d76e3c16c4896f49f55872aff4faa707f5`
and `eb3a72042cfdfc9cbd6d182055e0f5f00719c10ea5945a41c598ed2c83ad69c4`.
Preserve endpoint-specific protocol/region qualification and existing cache
semantics. The full historical field set is preserved rather than newly
certified; recorded source/qualification gaps remain outstanding.

## Incremental review: 2026-11-09

The fixed models.dev payload is
`06f904263b196dae1281b28f95963fa2fec04f0cba6d8205af13827746833412`,
sampled November 8 at 20:35:16 UTC for the November 9 Beijing cycle.
The [endpoint audit](endpoint-review.md#2026-11-09-incremental-review)
records the two affirmative Fireworks Serverless retirements and all
30 special endpoints.

The independently sampled
[OpenRouter listing](https://openrouter.ai/api/v1/models) agrees with these
five discovery output projections; both imported output fields change, without
changing reviewed static defaults or certifying a server-side request default:

| OpenRouter ID | Previous | Current |
| --- | ---: | ---: |
| `deepseek/deepseek-v4.1-flash-1017` | 393216 | 943718 |
| `meta-llama/llama-3.1-8b-instruct` | 117964 | 16384 |
| `meta-llama/llama-3.2-3b-instruct` | 72000 | 117964 |
| `qwen/qwen3.6-27b` | 65536 | 81920 |
| `z-ai/glm-5.3-flash` | 943717 | 131072 |

Three Mercury records are absent from both listings. Four Fireworks discovery
records are absent from models.dev and appear in the official November 7
Serverless retirement table: GPT OSS 120B, Inkling, Nemotron 3 Ultra and
Nemotron 3.5 Lightning. Only the first and third had static public registrations.
Missing discovery records do not themselves revoke static routes.

The two new Fireworks artifact records remain `remote_discovered`:

- [Kimi K3.1 Pro](https://fireworks.ai/models/fireworks/kimi-k3p1-pro):
  the exact card/index corroborates public Serverless, context 1048576,
  tools and image. It does not substantiate the raw output 131072 or the
  reasoning/toggle/effort claims for this exact model. The already-reviewed
  static route keeps unknown output default/ceiling and unverified reasoning.
  The runtime 8192 fallback is not a provider default.
- [Ultra-fast Preview](https://docs.fireworks.ai/serverless/serverless-modes):
  the exact router ID and Chat API endpoint are documented. Raw context
  1000000 differs from the
  [base-model card/index](https://fireworks.ai/models/deepseek-ai/deepseek-v4p1-flash-1017)
  value 1048576; the router guide gives no independent capacity. Raw output
  384000 and a request default are not established. Do not copy those fields
  or series-level reasoning rules into a static router contract.

Six raw Fireworks records only drop `budget_tokens` with minimum 1024:
Ember 1, Kimi K3, and the `kimi-fast-latest`, `kimi-k3-fast`, `kimi-k3-spot`,
`kimi-latest` routers. Their other complete raw fields are unchanged.
Generic Chat schema budget support is not exact-model evidence, so it does not
justify restoring these options. The Kimi K3 API's Low/High/Max semantics
also do not imply four independent native tiers merely because medium is
accepted and promoted to high. Existing static effort/defaults are unchanged.

OpenCode's Frantic entry is Zen-only raw evidence, not Go protocol admission.
The two independently fetched Zen envelopes differ in all 81 `created`
timestamps but otherwise match; neither contains the exact candidate ID.
All unrelated generated objects and the remaining 630 reviewed effective
facts, raw inputs and status fields are protected by complete-object comparison.

## Incremental review: 2026-11-08

The fixed models.dev payload is
`6a3f0abfcc9a357d8f29230659697e4ba1a3793ebebb3d9838ecd4160e6f84a6`,
sampled November 7 at 20:37:27 UTC for the November 8 Beijing cycle.
The [endpoint review](endpoint-review.md#2026-11-08-incremental-review)
records the separate 30-endpoint audit; none needs a new static route.

The [official OpenRouter listing](https://openrouter.ai/api/v1/models),
raw SHA-256
`4a3fd5723e060683708fbd53caf5475d95ad496dab64e9b73b7c8c6936550f28`,
independently agrees with these models.dev discovery projections:

| OpenRouter model ID | Reviewed projection |
| --- | --- |
| `liquid/lfm-3-1.7b` | New text-only discovery record; context 262144, output ceiling 8192, tools and reasoning. |
| `liquid/lfm-3-1.7b:free` | Separate free discovery record with the same capacity/modality fields. |
| `minimax/minimax-m2.5` | Output projection 32768 → 58982 from `top_provider.max_completion_tokens`; context unchanged. |
| `moonshotai/kimi-k2.5` | Output projection 235929 → 65536 from the same provider-local field; context unchanged. |

The existing adapter also populates `default_max_output_tokens` from that
output ceiling. This remains a discovery projection, not newly established
request-omission behavior or a subscription entitlement. Liquid's listing
warns that prompts and outputs may be retained and used; discovery does not
change provider selection or account settings.

For `venice/deepseek-v4-1-flash-1017-ultrafast`, the fixed models.dev object
adds `image` to input modalities. Only the remote discovery record's
`image_input` changes to true; capacity, effort, tools and defaults are
unchanged. No independent authenticated Venice qualification or inference
was performed, and this does not certify another provider's DeepSeek route.

### Correct Crouton review provenance without changing facts

The two pre-existing `opencode-go@default/crouton-alpha-free` and
`opencode-go@messages/crouton-alpha-free` review annotations accidentally
described native Chat Hy4 and translated Messages/Responses. Correct only
their reason and review date: Crouton is native **Responses**, while
**Chat/Messages** remain conservative translations. The newly sampled
[Go listing](https://opencode.ai/inference/go/v1/models) still has the same 30 IDs,
and its Crouton object still publishes the exact Responses base URL, image
input, context 1048576, output ceiling 270000 and
`minimal/low/medium/high/xhigh/max` efforts. The service default is unknown.

All 632 stored facts, inputs, status classifications, unknown fields and
execution budgets are retained. Regenerate the summary through the real
refresh command; do not edit its explanation separately. Neither correction
grants the translations capacity, vision, reasoning or agent-turn support.

## Incremental review: 2026-11-07

The fixed models.dev payload is
`91a77f08f7e88368aa75455a33e7074abcc04c330b1cb7b41c1778961c75a312`,
sampled November 6 UTC for the November 7 Beijing cycle. The
[endpoint review](endpoint-review.md#2026-11-07-incremental-review) records
the separate 30-endpoint evidence audit and preserved regional/plan boundaries.
The first four output-capacity projections below agree with the
[official listing](https://openrouter.ai/api/v1/models), sampled November 6
at 20:34:21 UTC, raw SHA-256
`6c6a59e12e87011eb5c4b1a26c0dde46ca10b8cca639fa011faaa9750239f6e3`:

| OpenRouter model ID | Reviewed projection |
| --- | --- |
| `deepseek/deepseek-chat` | Top-provider output 16,000 → 16,384. |
| `deepseek/deepseek-v4-flash-vision-exp` | Top-provider output 262,144 → 943,717. |
| `deepseek/deepseek-v4-pro` | Top-provider output 393,216 → 384,000. |
| `nvidia/nemotron-3-super-120b-a12b` | Top-provider output 262,144 → 16,384. |
| `ricursiveai/ripple-65b-preview` | Fixed models.dev display name becomes `Ripple 65B Preview`; the official listing names it `Ricursive: Ripple 65B Preview`. No capability or capacity change. |
| `nvidia/nemotron-3.5-content-safety:free` | Missing from both new discovery sources; remove only its remote projection, not static endpoint routes. |

Global context fields are unchanged even where `top_provider.context_length`
differs: the adapter uses the listing's global context. The five newly listed
OpenRouter `:batch` IDs are absent from the fixed models.dev payload and are
not admitted as real-time agent routes. These remote output projections are
not independent certifications of service defaults.

The fixed Venice source adds `deepseek-v4-1-flash-1017-ultrafast` as a
discovery-only record: context 1,000,000, output projection 131,072,
text input, reasoning and tools, effort `none/low/high/max`. It is not a
special-plan grant or a separately exercised native service contract.
Supplemental membership and all existing static/default mappings are preserved.

### Go native Crouton versus conservative translations

The [official Go model object](https://opencode.ai/inference/go/v1/models)
(November 6, 20:35:11 UTC; raw SHA-256
`cbc5fe208831cde8ffe71d425079e038dbe1ae5ea9c0a204dbc2acef82792e18`)
publishes exact ID `crouton-alpha-free`, name `Crouton Alpha Free`, and only
`api.responses`. Its base URL matches the registered Go Responses endpoint.
This independently supports the provider-local native route:
context 1,048,576; output ceiling 270,000; image input, tools and reasoning;
effort `minimal/low/medium/high/xhigh/max`. Do not invent a service output
default, cache control, parallel tool use, video/PDF certification or identity
with another provider's Crouton model.

The two existing-policy translations use `opencode-go@default` and
`@messages`; capacity and output defaults/ceilings stay unknown, image input
and reasoning stay false, effort stays empty, explicit cache control stays
false, and native tool metadata does not certify translated agent turns.
All previous preferred model/route selections must remain byte-for-byte
equivalent; only the new model gains its own Responses preferred mapping.

Codex's fixed upstream revision and model body are unchanged. DeepSeek's
slashless pricing request returned a home-page body despite HTTP 200;
the canonical trailing-slash pricing body and Responses documentation
were separately verified unchanged. A fallback response is not evidence
for deleting pricing or capability fields.

## Incremental review: 2026-11-06

The immutable models.dev payload is
`3f019fca55828f87d462e6140e635dc0548a5961f66e9f059026521912214ebc`,
sampled November 5 UTC for the November 6 Beijing cycle.
The [official OpenRouter listing](https://openrouter.ai/api/v1/models),
sampled November 5 at 20:34:06 UTC (454 records; raw SHA-256
`c82b1a41012f7bf702d57523bcfb78b7e033eec393a3806d482d2138bb9f114e`),
independently supports the following discovery-local changes:

| OpenRouter model ID | Reviewed projection |
| --- | --- |
| `google/gemini-omni-1.1-flash`, `google/gemini-omni-1.2-flash` | New discovery records; global context 131,072, top-provider output ceiling 65,536, image input, reasoning. The fixed models.dev objects specify `low/high` effort and explicitly disable tool calls. |
| `inclusionai/ling-3.0-tiny` | New discovery record; global context 262,144, top-provider output ceiling 250,000, text-only input, reasoning and tools. Toggle reasoning does not establish an effort enum. |
| `minimax/minimax-01`, `minimax/minimax-m2`, `minimax/minimax-m2-her`, `minimax/minimax-m2.1` | Absent from both fresh discovery sources; remove only their remote projections. Existing static routes are not retired by a listing omission. |
| `meta-llama/llama-3.3-70b-instruct`, `meta-llama/llama-4-maverick` | Top-provider output ceiling 16,384 → 32,768. |
| `openai/gpt-oss-20b` | Top-provider output ceiling 32,768 → 117,964. |
| `qwen/qwen3-235b-a22b-2507` | Top-provider output ceiling 16,384 → 117,964. |
| `qwen/qwen3-next-80b-a3b-instruct` | Top-provider output ceiling 235,929 → 16,384. |
| `google/gemma-4-26b-a4b-it` | Top-provider output ceiling 32,768 → 235,929. |
| `reflection/beam-1.0-501b-a23b` | Top-provider output ceiling 131,072 → 943,718. |
| `inference-net/schematron-v2-small` | Top-provider output ceiling 4,096 → 8,192. |
| `minimax/minimax-m2.7` | Top-provider output ceiling 131,072 → 176,947. |
| `ricursiveai/ripple-65b-preview` | Global context 16,384 → 1,048,576 and top-provider output ceiling 14,745 → 131,072. The 95% compact threshold follows the global context. |

These are aggregator discovery projections, not new static agent routes or
subscription entitlements. Output ceilings populate both discovery output
fields under the existing projection contract; they are not newly certified
vendor defaults. Global `context_length` is not replaced by
`top_provider.context_length`: several top-provider contexts changed without
changing the corresponding global context. The official-only `openrouter/auto-code`
record is not in the fixed models.dev source and does not change Kilo auto
mapping. No audio/video execution or tool capability is inferred from Omni's
name or description. In particular, the discovery projector's existing
`agent_turn` field is not certification that these tool-less Omni models can
execute agent turns, and does not grant a new static route.

The same three additions are present in Kilo's non-auto discovery listing,
not in any of its five auto routing pools. Both Omni objects omit tools;
Ling's tool parameters remain aggregator-local evidence. Kilo's existing
decoder can expose these candidates with inherited capability defaults,
which is not static admission, native endpoint support or live certification.
The five complete auto objects are unchanged. Fireworks' Ripple index and
[model card](https://fireworks.ai/models/fireworks/ripple-65b-preview) confirm
Function Calling and explicitly publish the base-model resource path
`accounts/fireworks/models/ripple-65b-preview`. The index sets `serverless=false`
and `serverlessApi=false`; the card also marks Serverless unsupported and
offers On-demand deployment only. A published base-model path does not establish
direct callability on the registered public serverless endpoint, so no static
route is admitted. Its abbreviated marketing capacities are not copied into
other providers.

The artifact changes from 1,076 to 1,075 models. Its complete route, alias and
preferred/default collections remain identical. All 55 retained supplemental
models, the complete built-in snapshot, and every fact, protected input and
status in the 629-record metadata review baseline remain unchanged. Mapped
upstream changes outside these OpenRouter limits are price fields, not
endpoint-specific capability evidence.

The [official Codex catalog at the fixed revision](https://github.com/openai/codex/blob/126a0ee92ed2c5e73cd8a8f7e37563e8527552d7/codex-rs/models-manager/models.json)
was sampled November 5 at 20:34:02 UTC, raw SHA-256
`1384625c0b6b3e67d3c60da29becea2b017f2c283fc1e2d05b8edafb4698eb10`.
All 12 objects are byte-for-byte unchanged; generic API additions do not
establish Codex access.
[DeepSeek pricing](https://api-docs.deepseek.com/quick_start/pricing/) and
[Responses](https://api-docs.deepseek.com/guides/responses_api/) were freshly
sampled at 20:34:04/05 UTC; normalized bodies match the previous cycle exactly.
No capacity, effort, endpoint or account-access change is inferred.

## Incremental review: 2026-11-05

The immutable models.dev payload is
`a032bd9630156a5a601e8097d13005838c9be0822e37b1e0cb035af10e6e79ca`.
The [official OpenRouter listing](https://openrouter.ai/api/v1/models),
sampled November 4 at 20:34:48 UTC (raw SHA-256
`91afc2ddc6864809142b8ae100f26ea641936681a69790a62ffc587bf54e4b9e`),
supports these endpoint-local discovery projection changes:

| OpenRouter model ID | Field | Previous | Current |
| --- | --- | ---: | ---: |
| `meta-llama/llama-3.3-70b-instruct` | output ceiling | 128,000 | 16,384 |
| `nvidia/nemotron-3.5-super-vl` | output ceiling | 235,929 | 943,718 |
| `openai/gpt-oss-20b` | output ceiling | 117,964 | 32,768 |
| `qwen/qwen3-235b-a22b-2507` | output ceiling | 117,964 | 16,384 |
| `z-ai/glm-5.3-flash` | output ceiling | 131,072 | 943,717 |
| `xiaomi/mimo-v2.5` | model-level context | 1,050,000 | 1,000,000 |

The existing remote projection also uses these output ceilings as defaults;
this does not independently certify an omitted-request service default.
GLM's model-level context is `1048576`, while its provider-level context is
`1048575`; neither is substituted for the other. MiMo's compaction projection
changes from `997500` to `950000` with its model-level context.

Five new remote records are `chutes/tencent/Hy4`,
`openrouter/ricursiveai/ripple-65b-preview`,
`openrouter/stealth/crouton-alpha`, `openrouter/tencent/hy4`, and
`together/moonshotai/Kimi-K3.1-Pro`. These remain discovery metadata, not
static entitlement or independent certification of every upstream field.
The two missing `openrouter/thinkingmachines/inkling:free` and
`openrouter/thinkingmachines/inkling-small:free` records are removed only
from the generated discovery artifact, not interpreted as provider-wide EOS.

### DashScope Messages and Tencent Hy4

The [DashScope Messages contract](https://help.aliyun.com/zh/model-studio/anthropic-api-messages)
(raw SHA-256 `eb2d5c64ed1a790daf2423632d63c509e78fbaf3c460ea140623f185a7afdf39`)
adds exact `kimi-k3.1-pro`, default-on thinking and
`output_config.effort=low/medium/high/max` (service default `max`).
The [Kimi deployment guide](https://help.aliyun.com/zh/model-studio/kimi-api)
independently certifies text/image input, not video. Only the ordinary
DashScope Messages endpoint is eligible; no Token Plan or Coding Plan
entitlement, integer capacity, output default or explicit cache breakpoint
support is inferred. Effort must be sent as `output_config.effort`, not
translated into an Anthropic thinking-token budget.

Tencent's [protocol matrix](https://cloud.tencent.com/document/product/1823/130079)
and [Hy4 guide](https://cloud.tencent.com/document/product/1823/132252)
(raw SHA-256 `6132e8361d25a0f7cc461ddf20f56565ff56fae6899f30e8cd75626280c91622`)
certify exact `hy4` in Chat, Messages and Responses, including image input
independently for all three protocols. The
[thinking contract](https://cloud.tencent.com/document/product/1823/131208)
(raw SHA-256 `3d02637b55382a09f151ba923f6a70a22d504a66838f98d0d49a14365116d098`)
certifies Chat `high/no_think`. Messages/Responses retain conservative
reasoning=false and no effort controls without separate protocol evidence;
that restriction must not erase their independently certified image input.
Abbreviated `1024k/1M/960k/64k` are not exact integer budgets or defaults.

The same exact Chat contract replaces `none` with `no_think` for
`hy4-preview` and `hy3`, and adds `none` to exact `deepseek-v4-pro`.
It does not change similarly prefixed/suffixed IDs or another provider.
Provider configuration and final route/model validation are separate:
accepting Tencent Chat's `no_think` must not relax OpenAI's generic
vocabulary or bypass the selected model's allowlist. The old spelling was
not tested against a paid service. `hy4-preview` remains a distinct ID.

### Go and Fireworks provider-local additions

The [Go listing](https://opencode.ai/inference/go/v1/models), sampled
November 4 at 20:35:12 UTC (raw SHA-256
`b0f53924e8d397bb5c2d55040f642012b9dedbf71b2c4b772b99d03374adae75`),
adds exact `hy4` alongside `hy4-preview`. Native Chat context is `1048576`,
input limit `983010`, output ceiling `65536`, image/tools/reasoning true,
and effort `none/high`; omitted-request output default remains unknown.
Messages/Responses are translated text routes, not separately certified
native metadata: budgets remain unknown, image/reasoning and effort are
disabled, and the new translated routes do not claim agent-turn support.
Preview's privacy and training statements are not transferred to Hy4.

The [Fireworks index](https://fireworks.ai/models) independently publishes
integer `contextLength="1048576"` and image input for exact
`accounts/fireworks/models/kimi-k3p1-pro`. Its
[deployment card](https://fireworks.ai/models/fireworks/kimi-k3p1-pro)
(raw SHA-256 `0b26b7527390c47b1e7ea2920ac67f1b0127797b87f4ebed0abe617e6d5caf4e`)
confirms serverless availability and Function Calling, not an account's
quota or successful inference. Output default/ceiling and model-specific
effort remain unknown; reasoning is not promoted without independent
evidence. Input-cache pricing is not an explicit cache-write contract.

Together's [K3.1 card](https://www.together.ai/models/kimi-k3-1-pro)
only establishes the exact `moonshotai/Kimi-K3.1-Pro` deployment,
text/image and abbreviated `1M`. It remains discovery-only; no Fireworks,
Moonshot or Go budgets or effort are copied into a static Together route.

## Incremental review: 2026-11-04

The frozen models.dev payload is
`0d6995e11f1174c560cf6dd7bf7458e53258ea0b4de19dc8072e9b3e972051c8`.
The [official OpenRouter listing](https://openrouter.ai/api/v1/models),
sampled on November 3 at 20:34 UTC (raw SHA-256
`c690e60e063d18d7e69556ba2e1fb06ff47dff6f54e0254ee3e0b7d4fa293bef`),
independently supports six provider-level output projection changes:

| OpenRouter model ID | Previous | Current |
| --- | ---: | ---: |
| `google/gemma-4-26b-a4b-it` | 235,929 | 32,768 |
| `meta-llama/llama-3.1-70b-instruct` | 16,384 | 8,192 |
| `meta-llama/llama-3.2-3b-instruct` | 117,964 | 72,000 |
| `meta-llama/llama-3.3-70b-instruct` | 16,384 | 128,000 |
| `qwen/qwen3-235b-a22b-2507` | 16,384 | 117,964 |
| `qwen/qwen3-next-80b-a3b-instruct` | 16,384 | 235,929 |

The existing remote projection uses these ceilings as both default and upper
bound; this is not a newly certified omitted-request default. Model-level
context and `top_provider.context_length` remain separate. New Crouton and
Grok batch records in the public listing do not themselves add static routes.

Ten Venice output projections also change in the upstream payload:
six exact DeepSeek IDs (`deepseek-v4-1-flash[-1017]`,
`deepseek-v4-flash[-0731]`, `deepseek-v4-pro[-0813]`) now report 262,144;
the two Gemma 4 26B/31B IDs report 16,384; `minimax-m3-preview` reports
131,072; and `nvidia-nemotron-3-5-super-vl-120b-a12b` reports 102,400.
**These ten changes have models.dev provenance only in this cycle.**
Both public Venice model-list URLs and the independent official model-list
and reasoning documentation failed with connection resets on November 3.
Keep the records `remote_discovered`, with this first-party verification
debt explicit; do not certify them as native-vendor hard limits, change a
static route, or transfer them to another endpoint. Existing remote default
projection behavior is unchanged.

The upstream `xai/grok-imagine-image-quality` remote record disappears, but
the independent [official migration notice](https://docs.x.ai/developers/migration/imagine-image-quality-nov-2)
expressly preserves requests to that slug after November 2 by redirecting
generation and editing to `grok-imagine-image-2.0` with `quality: "low"`.
It preserves request/response shapes and reports the serving model in the
response. Keep the existing reviewed static route; deletion of the remote
record does not remove that compatible alias or add a quality selector.
This is a compatibility redirect for a retired model slug, not continued
service by the original model or a promise of its old pricing. The explicit
shape-and-parameter compatibility guarantee supports the existing capability
subset; it does not admit new editing or quality controls.
The notice was sampled on November 3 at 20:45 UTC, raw SHA-256
`b0d61b1bf32f97b2e1dfccf47fbd063ac234464d42b47d7333bde345931422db`.

### Exact Tencent TokenHub retirements

The [protocol matrix](https://cloud.tencent.com/document/product/1823/130079)
and [DeepSeek guide](https://cloud.tencent.com/document/product/1823/132248)
removed the two old bare Flash IDs on November 3. Independent final service
notices, not model-table omissions, confirm their retirement and reveal
previously missed historical retirements:

| Exact TokenHub ID(s) | Final cutoff, Beijing time | Official notice |
| --- | --- | --- |
| `deepseek-v3.2` | July 16, 2026, 00:00 | [2327](https://cloud.tencent.com/announce/detail/2327) |
| `minimax-m2.5` | August 7, 2026, 00:00 | [2362](https://cloud.tencent.com/announce/detail/2362) |
| `hy3-preview` | August 31, 2026, 00:00 | [2391](https://cloud.tencent.com/announce/detail/2391) |
| `kimi-k2.5` | August 31, 2026, 00:00 | [2414](https://cloud.tencent.com/announce/detail/2414) |
| `qwen3.5-flash`, `qwen3.5-plus` | September 8, 2026, 00:00 | [2427](https://cloud.tencent.com/announce/detail/2427) |
| `deepseek-v4-flash` | September 27, 2026, 00:00 | [2451](https://cloud.tencent.com/announce/detail/2451) |
| `glm-5`, `glm-5-turbo`, `glm-5.1` | October 9, 2026, 00:00 | [2469](https://cloud.tencent.com/announce/detail/2469) |
| `youtu-vita` | October 15, 2026, 00:00 | [2447](https://cloud.tencent.com/announce/detail/2447) |
| `mimo-v2.5-pro` | October 20, 2026, 23:59:59 | [2494](https://cloud.tencent.com/announce/detail/2494) |
| `glm-5v-turbo` | October 30, 2026, 23:59:59 | [2479](https://cloud.tencent.com/announce/detail/2479) |
| `deepseek-v4-flash-0731` | October 31, 2026, 00:00 | [2496](https://cloud.tencent.com/announce/detail/2496) |

All cutoffs precede the November 3 sample. The two Flash IDs account for six
current routes; the other twelve IDs account for 27 historical omissions,
not 27 newly retired November 4 routes. Limit removal to the fourteen exact
`tencent-tokenhub` model IDs and their 33 existing routes (14 Chat,
14 Messages, five Responses). The official [model list](https://cloud.tencent.com/document/product/1823/130051)
links the MiMo display name to its lowercase call ID. Conditional upgrades
for eligible accounts, sometimes with changed billing, do not certify the
old model or guarantee that every account's old request still succeeds.
Some notices also describe automatic switching, but do not provide the
equivalent request/response-shape and accepted-parameter guarantee above.
YT-VITA availability on a different image-recognition API is not TokenHub
availability.

Retain native-direct prefixed/suffixed Flash IDs, other providers' models,
and Tencent M3/M3.1, Pro and V4.1. In particular, the
[M2.7 cutoff](https://cloud.tencent.com/announce/detail/2511) is
November 17, 2026, 23:59:59 Beijing time, still in the future.

The exact V4.1 Chat IDs `deepseek-v4.1-flash` and
`deepseek-v4.1-flash-1017` have thinking instructions and the
`low/high/max` effort set in the DeepSeek guide; `medium/xhigh` are
compatibility mappings to `high`, not additional strengths. Enable only
their reviewed Chat reasoning and effort policy; keep Messages/Responses
reasoning explicitly disabled rather than inheriting the model flag.
Capacity abbreviations such as 1M/384K do not provide unambiguous integer
capacities or omitted-request defaults, so those unknowns remain null.
The guide's sampled raw SHA-256 is
`4b67e9cb864cc52d705e94b0a9ac8f154ea0f99293218984ba7c690620d6ce9a`.

MiniMax M3.1 `open_weights` changes are upstream descriptive metadata, not
new subscription entitlement, protocol support or runtime capability.

## Incremental review: 2026-11-03

The frozen models.dev payload is
`dbfacaf2b8140dadc0be5a1354bb4a4284c24d01814e3370865817d727e2d76a`.
The [official OpenRouter listing](https://openrouter.ai/api/v1/models),
sampled on November 2 at 20:36 UTC, independently supports these remote
projection changes:

| OpenRouter model ID | Field | Previous | Current |
| --- | --- | ---: | ---: |
| `meta/muse-spark-open` | Provider output ceiling | 131,072 | 943,718 |
| `qwen/qwen3.8-27b` | Model-level context | 1,000,000 | 1,024,000 |

The existing projection uses the Muse provider ceiling as both its default
and upper bound; this is not a newly certified omitted-request default.
Qwen's existing 95% auto-compaction rule gives 972,800 instead of 950,000.
Model-level and provider-level capacities are not interchangeable.

The new `openrouter/z-ai/glm-5.3-flashx-legal` remote discovery record has
exact-ID support for context 1,048,576, provider output 131,072, image input,
tools, reasoning and the `low/high/max` effort set. It adds no static route
or subscription entitlement. Its advertised video input does not add a
runtime video capability. OpenRouter's Hermes 3 405B/70B and Venice's Hermes
3 405B records disappeared from the snapshot; removing those remote records
does not establish retirement of a reviewed static route.

Two independent official
[Coding Plan](https://ark.volcengine.com/region:cn-beijing/docs/coding-plan-personal-model-deprecation.md)
and [Agent Plan](https://ark.volcengine.com/region:cn-beijing/docs/agent-plan-personal-model-deprecation-announcement.md)
notices were re-read on November 2 at 20:34 UTC. Both retain the final
MiniMax M3 shutdown time of **November 2, 2026, 14:00 Beijing time
(06:00 UTC)**, already past at this sample. Remove only
`volcengine@coding/minimax-m3` and `volcengine@plan/minimax-m3`, and prevent
old supplemental routes from restoring them. Do not infer ordinary-API,
Tencent or Go retirement. Keep the separately supported `minimax-latest`
and `kimi-latest` aliases; their old example mappings alone do not retire
the aliases. Surviving route metadata and provider defaults stay unchanged.

The Coding notice's raw SHA-256 is
`6b89ef9880ae40162f809109053e615dd17c705a777ab9ff214808cd7d9afa30`;
the Agent notice's is
`805264fec5c9fd73f7b5e91341a48ece7e8c392dc9f46b540f49904c829bfbc2`.
Both returned HTTP 200. These are affirmative endpoint-specific notices,
not a conclusion drawn from a missing list entry.

Codex's immutable upstream catalog and normalized DeepSeek pricing/Responses
documentation are unchanged from the previous sample. Preserve the exact
OpenRouter Mistral Large 4.0 `none/high` exception and the unresolved native
version/default limits; generic API changes do not certify Codex or plan
eligibility.

## Incremental review: 2026-11-02

The frozen models.dev payload is
`a2e417bd2a17eb0839af84dcc16280542da492809502f2faedf86ba0a52cc911`.
The [official OpenRouter listing](https://openrouter.ai/api/v1/models),
sampled on November 1 at 20:42 UTC, independently confirms two changed
provider output ceilings:

| OpenRouter model ID | Previous projection | New projection |
| --- | ---: | ---: |
| `openai/gpt-oss-20b` | 32,768 | 117,964 |
| `~deepseek/deepseek-v4-flash-latest` | 131,072 | 943,718 |

The existing remote projection uses each ceiling as both its default and
upper bound; this cycle does not introduce a new defaulting policy or
certify an omitted-request default. Model-level context fields, static
route budgets, aliases and preferred mappings remain unchanged.

The same listing explicitly gives `mistralai/mistral-large-4-0` the effort
set `high/none`. The frozen upstream instead changed its effort values
from `none/high` to `low/medium/high`. Preserve the endpoint's reviewed
`none/high` contract at the shared projection boundary, for this exact
OpenRouter model only. Do not mutate the raw payload or infer a native
Mistral, other-gateway, or future-version contract from this exception.

Independent native Mistral evidence partly recovered through a browser
transport while all 14 local Mistral URL reads reset. The
[native reasoning reference](https://docs.mistral.ai/studio/conversations/reasoning)
gives Large 4 `low/medium/high` and Small 4 `none/high`; neither overrides
the separately sampled OpenRouter contract. The
[Large 4 family card](https://docs.mistral.ai/models/mistral-large-4-0)
does not establish the exact `mistral-large-4-0-8` binding or its output
default/ceiling. Keep that candidate gated and the raw snapshot intact.
Browser excerpt hashes attest only to the saved excerpts, not complete
upstream response bytes. No new native Mistral capacity is certified here.

The immutable [Codex model catalog](https://github.com/openai/codex/blob/126a0ee92ed2c5e73cd8a8f7e37563e8527552d7/codex-rs/models-manager/models.json)
remains byte-identical, with 12 slugs. DeepSeek's
[pricing](https://api-docs.deepseek.com/quick_start/pricing/) and
[Responses guide](https://api-docs.deepseek.com/guides/responses_api)
have unchanged extracted public text. No generic API field is copied to
Codex or an unrelated protocol, region or subscription endpoint.

## Incremental review: 2026-11-01

The frozen models.dev payload is
`30f6fb19affa533339b2930bafacbc59fb4ee2c4e034288e8e972f31a82d5e9a`.
Only eight remote-discovered OpenRouter records change; all other artifact
fields and every route, alias and preferred mapping are unchanged. The 55
supplement models, 16 explicit supplement routes and 656 reviewed route
projections remain unchanged. These are artifact refreshes, not new static
endpoint eligibility or runtime output-default policies.

The [official public listing](https://openrouter.ai/api/v1/models), sampled
again on October 31 at 20:43 UTC, independently confirms these output ceilings:

| OpenRouter model ID | Previous artifact | Frozen artifact / official resample |
| --- | ---: | ---: |
| `deepseek/deepseek-v4-pro-0813` | 393,216 | 943,718 |
| `qwen/qwen3.6-27b` | 81,920 | 65,536 |
| `minimax/minimax-m2.5` | 131,072 | 32,768 |
| `~deepseek/deepseek-v4-flash-latest` | 943,718 | 131,072 |

Both the frozen upstream and the official listing omit
`poolside/laguna-s-2.1`, `poolside/laguna-s-2.1:free`,
`poolside/laguna-xs-2.1` and `poolside/laguna-xs-2.1:free`.
Remove only their generated remote-discovered records. Absence is not
affirmative model-wide retirement evidence; no static route or user
configuration is deleted.

Preserve the sampling distinction: the earlier 20:35 UTC official listing
still gave Flash Latest a 943,718 ceiling; the 20:43 resample agrees with
the frozen upstream's 131,072. Both samples give M2.5 a model-level
`context_length` of 204,800 and a `top_provider.context_length` of 198,000.
These are different aggregation levels, not a temporal context reduction.
The unchanged artifact retains the model-level 204,800; a provider-level
value neither rewrites that field nor certifies every provider's capacity.
The [Flash Latest endpoints response](https://openrouter.ai/api/v1/models/~deepseek/deepseek-v4-flash-latest/endpoints)
has no endpoints; this does not establish alias retirement.

The immutable [Codex model catalog](https://github.com/openai/codex/blob/126a0ee92ed2c5e73cd8a8f7e37563e8527552d7/codex-rs/models-manager/models.json)
is byte-identical to the previous sample, including all 12 slugs.
DeepSeek's [pricing](https://api-docs.deepseek.com/quick_start/pricing/) and
[Responses reference](https://api-docs.deepseek.com/api/create-response)
also have unchanged extracted public text. No generic API field is copied
to Codex or an unrelated protocol, region or subscription endpoint.

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
The review-only [endpoint association](../crates/holon-model-catalog/data/endpoint-mappings.json) therefore binds
only the CN provider. `auto` remains a dynamic selector without a reviewed
uniform numeric capacity; no underlying model's limits are copied into it.

### DeepSeek Responses

`deepseek@responses/deepseek-flash` receives **1000000 / 384000**, preserving
the existing conservative reviewed ceiling in
[`deepseek_v1.json`](../crates/holon-model-catalog/src/models_dev/manifests/deepseek_v1.json).
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
