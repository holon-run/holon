# Endpoint-specific catalog review

Initial review: **2026-10-16 Beijing time**; subsequent cycles are recorded below. This is an
evidence record, not a claim that every account, region, subscription tier, or
model was exercised. No credentials were read and no paid inference was called.

The general API catalog is not a subscription allowlist. Preserve previously
reviewed routes unless there is affirmative removal evidence. New routes require
their own endpoint evidence; missing capability flags are conservative omissions,
not proof that a model lacks the feature.

## 2026-10-22 incremental review

- Refreshed models.dev revision
  `fd70d4edf75e6b29b624a2683b316bac61a871d5b0ca6122e4fbe4e99f7f41de`,
  fetched on October 22 at 14:33 UTC.
  Relative to the last merged catalog, the artifact contains 1,091 models
  (nine additions, 63 removals, 97 changed records). This includes the
  previous cycle's unsubmitted increment. The supplement retains 51 models
  and 16 explicit routes, with no new drafts. The existing `mimo-v2.6-pro`
  metadata and its Token Plan route move into the built-in catalog without
  changing their effective token defaults or reviewed eligibility.
  Most changes are upstream-only aggregator descriptions, token limits, or
  reasoning options; 56 removals are NVIDIA upstream records, not affirmative
  model-retirement evidence or deletions from the reviewed runtime catalog.
  The artifact is not loaded directly by the runtime.
  No general API addition establishes subscription eligibility.
- All 20 added/changed OpenRouter records match the
  [public models API](https://openrouter.ai/api/v1/models) for context and
  `top_provider.max_completion_tokens`, rechecked on October 22 at 14:23 UTC.
  This comparison is deliberately limited to changed records.
- The [published Codex model metadata at revision d63a9b83](https://github.com/openai/codex/blob/d63a9b8344cfe58bc78bbe319b560378fc8756ef/codex-rs/models-manager/models.json)
  is byte-identical to the previous cycle, and GitHub's path-specific commit
  history now confirms that revision. Existing Codex definitions cover all
  seven visible, non-retired models, using the published 272,000 default
  context rather than the optional 872,000 maximum or general API limits.
  The [Codex lifecycle](https://learn.chatgpt.com/docs/models) still confirms
  the six previously excluded ChatGPT-sign-in retirements; no new Codex
  model or route change is established.
- Rechecked [DeepSeek pricing](https://api-docs.deepseek.com/quick_start/pricing)
  and the [Responses guide](https://api-docs.deepseek.com/guides/responses_api/).
  Flash legacy names remain accepted aliases, not new model versions;
  published context/output units remain 1M/384K. The supplement's sole
  model-metadata change remains the prior cycle's models.dev integer
  normalization for `deepseek-v4-flash-vision-exp`, not independent proof
  of an exact context integer. Endpoint-specific capability and search
  restrictions remain separate from that upstream metadata.
- [Xiaomi's retirement notice](https://mimo.mi.com/docs/en-US/updates/deprecate)
  ended native `mimo-v2.5-pro` and `mimo-v2.5` service on **October 21,
  2026 at 10:00 Beijing time**. Remove both native API models and Token Plan
  routes, and reject these exact IDs when drafting or retaining Xiaomi
  supplement entries. This does not retire Tencent/OpenRouter offerings,
  Ultraspeed variants, or audio IDs. The [current model table](https://mimo.mi.com/docs/en-US/quick-start/summary/model),
  [Token Plan table](https://mimo.mi.com/docs/en-US/tokenplan/Token%20Plan/subscription),
  [Codex configuration](https://mimo.mi.com/docs/en-US/tokenplan/integration/codex-configuration),
  and [OpenCode configuration](https://mimo.mi.com/docs/en-US/tokenplan/integration/opencode)
  independently retain the existing V2.6 Pro/Flash eligibility, Responses
  protocol, image/reasoning/tool configuration, and 1,048,576/131,072 example
  limits. Configuration examples are not independent service hard limits;
  no V2.6 Pro Ultraspeed subscription route is inferred. Promote the already
  reviewed `mimo-v2.6-pro` entry and Token Plan route into the built-in catalog
  so both providers retain a non-retired preferred route. This preserves
  credential-only automatic model selection; user defaults and fallbacks are
  not rewritten. The promoted description preserves the client-example
  evidence boundary rather than presenting those defaults as service limits.
- Volcengine's [Coding Plan](https://docs.volcengine.com/docs/ark/coding-plan-personal-plan-overview?lang=zh)
  and [Agent Plan](https://docs.volcengine.com/docs/ark/agent-plan-personal-plan-overview?lang=zh)
  independently add `deepseek-v4.1-flash-economy`. Add only these two
  subscription routes, with documented image/reasoning support; do not
  infer a default API route. Published 1049k/384k units do not establish
  exact integer limits or an effort enum, so those fields remain unset.
  The `code-plan` spelling returned only a JavaScript shell in this check;
  its body equivalence to `coding-plan` remains unverified.
- Tencent's [protocol matrix](https://cloud.tencent.com/document/product/1823/130079)
  independently supports `deepseek-v4.1-flash-1017` and
  `minimax-m3.1-flash` in Chat, Messages, and Responses. Add six routes;
  the separately reviewed Responses allowlist grows from 28 to 30.
  Exact TokenHub limits, capabilities, and effort controls remain unknown,
  rather than being inherited from the original vendors. Native Xiaomi
  retirement does not remove the matrix's `mimo-v2.5-pro` entry.
- Rechecked [mainland](https://help.aliyun.com/zh/model-studio/coding-plan)
  and [international](https://www.alibabacloud.com/help/en/model-studio/coding-plan)
  DashScope Coding Plan separately: the same six current IDs and their
  image eligibility remain documented, with distinct subscription keys
  and regional bases. Token Plan retains the union of the
  [personal](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview)
  and [team](https://help.aliyun.com/zh/model-studio/token-plan-team-overview)
  tables, including the already registered dated DeepSeek ID; absence
  from the personal table does not remove team eligibility. The six
  previously deferred Qwen/Kimi subscription routes remain unconfirmed.
- MiniMax [international](https://platform.minimax.io/docs/m-plan/quickstart.md)
  and [China](https://platform.minimaxi.com/docs/m-plan/quickstart.md)
  independently retain formal M3.1 Flash API/subscription support and key
  isolation. Its published Messages schema still specifies 131,072
  recommended output and a 524,288 ceiling, forced thinking, and five
  effort values; Preview requests remain compatibility aliases, not
  separate current products. [Z.AI](https://docs.z.ai/devpack/latest-model.md)
  and [BigModel](https://docs.bigmodel.cn/cn/coding-plan/latest-model.md)
  retain their independently reviewed model/modality/effort and key-type
  boundaries. No additional route change is established in these groups.
- [Step Plan](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api),
  [Kilo Auto](https://api.kilo.ai/api/gateway/models), and
  [OpenRouter Auto](https://openrouter.ai/api/v1/models) retain reviewed
  routes and limits; OpenRouter's completion ceiling is still null.
  [OpenCode Go](https://opencode.ai/v2/docs/console/go/) documentation is
  consistent with existing protocol registrations, but its public models
  endpoint returned 403: the model total was not reverified. The
  [Moonshot model table](https://platform.kimi.com/docs/models.md) still
  lists the four reviewed IDs, but an international-site 403 leaves
  regional eligibility partially unverified. Do not infer new domains,
  subscription rights, or historical removals from these gaps.

### Final verification

- Snapshot regeneration, refresh, and catalog validation passed: 1,091
  artifact models, 51 supplement models, and 16 explicit supplement routes.
  The promoted Xiaomi metadata is unique, both preferred routes are present,
  and retired native IDs are absent from the built-in catalog and supplement.
- Catalog tests: 133 passed, one ignored; configuration model-selection
  tests: seven passed; models.dev adapter tests: 11 passed.
- `cargo fmt --all -- --check`, `git diff --check`, and
  `RUSTFLAGS="-D warnings" cargo check --locked --all-targets` passed.
- Independent read-only review confirmed the Xiaomi automatic-selection
  repair and found no remaining confirmed blockers in the reviewed changes.
  This is source and offline verification, not live endpoint execution.

## 2026-10-21 incremental review

- Refreshed models.dev revision
  `7bd62f47cf6dbda6c70929f5ee5c398e9fc054d7f6dbb964d78eb18de5136b5e`.
  The artifact contains 1,141 models (two additions, six removals);
  the supplement retains 52 models with no new drafts or removals.
  General API additions/removals do not establish subscription eligibility.
- [DeepSeek Models & Pricing](https://api-docs.deepseek.com/quick_start/pricing)
  still documents 1M context and 384K maximum output. The refreshed artifact
  normalizes four DeepSeek context limits from 1,000,000 to 1,048,576;
  only `deepseek-v4-flash-vision-exp` changes in the supplement.
  These integer values follow models.dev; the official page publishes unit
  labels, not an independently verified exact integer context limit.
  Its compatibility notice still accepts `deepseek-v4-flash` and
  `deepseek-v4-flash-vision-exp`, backed by V4.1-Flash-1017 and billed as Flash.
  Preserve those aliases; do not infer new capabilities for legacy routes.
- This cycle's changed OpenRouter context/output limits match its
  [public models API](https://openrouter.ai/api/v1/models); this is not a
  full-catalog zero-difference claim. The existing `minimax/minimax-01`
  output-limit discrepancy is outside this incremental review.
  Other upstream changes include Together output limits, xAI reasoning
  options, and MiniMax integer output ceilings. The MiniMax M3.1-Flash
  ceiling matches reviewed built-in metadata; M3's direct-endpoint integer
  limit remains unverified rather than newly established by gateway data.
- Codex's fetched public model metadata is byte-identical to the previous
  cycle. The fetched lifecycle document records GPT-5.5's Codex retirement
  on October 14; the existing Codex routes already exclude it.
  GitHub API/raw TLS timeouts prevent establishing whether upstream main
  advanced after the captured metadata; do not claim a newer revision.
- The extra Token Plan models observed in the running catalog
  (`qwen3.7-flash`, `qwen3.8-omni-flash`, `kimi-k3`) come from the existing
  supplement, not missing built-in definitions. Their six Coding/Token Plan
  routes remain deferred/unconfirmed as recorded below. Preserve them pending
  affirmative removal evidence; general API metadata is not eligibility proof.

## 2026-10-20 incremental review

- Refreshed models.dev revision
  `3759647be3fd5d98270b03b5e834bc984ed06e897d55b504b5cd07b5685e6af0`.
  The artifact contains 1,145 models (eight additions, four removals).
  The sole new supplement draft, `minimax/MiniMax-M3.1-Flash`, is promoted
  to reviewed built-in metadata after the endpoint-specific checks below.
  Remove its duplicate supplement record before regeneration.
  Aggregator audit additions/removals do not create or remove runtime routes.
- Rechecked the [OpenRouter public list](https://openrouter.ai/api/v1/models).
  Both additions (`inception/mercury-3` and
  `minimax/minimax-m3.1-flash:free`) and all eight changed entries match
  advertised context/output metadata. The three removed OpenRouter IDs are
  absent from the public list. These are service metadata checks, not
  verified inference limits.
- Rechecked OpenAI's
  [published Codex metadata at revision d63a9b83](https://github.com/openai/codex/blob/d63a9b8344cfe58bc78bbe319b560378fc8756ef/codex-rs/models-manager/models.json):
  model objects are unchanged from the previously reviewed `8c076942`
  revision. Retain Codex-specific default contexts, reasoning options, and
  the existing preferred model. The
  [Codex lifecycle](https://learn.chatgpt.com/docs/models) still confirms
  the same six retired ChatGPT-sign-in IDs; no new entitlement is inferred.
- Rechecked the [DeepSeek Responses guide](https://api-docs.deepseek.com/guides/responses_api/):
  Flash image input and V4 Pro-only search remain consistent with the
  reviewed routes. No general API specification is copied to a subscription.
- MiniMax's international and China
  [Anthropic API](https://platform.minimax.io/docs/api-reference/text-anthropic-api.md)
  [references](https://platform.minimaxi.com/docs/api-reference/text-anthropic-api.md)
  independently identify the official `MiniMax-M3.1-Flash`, a **1,000,000**
  context and image/video input. The
  [Create Message schema](https://platform.minimax.io/docs/api-reference/text-chat-anthropic.md)
  confirms **524,288** maximum output and **131,072** recommended output,
  not models.dev's 512,000. Thinking is always on: efforts are exactly
  `low/medium/high/xhigh/max`, defaulting to `max`; `none` is rejected.
  If sent, `thinking.type` must be `adaptive`, not generic `enabled` with
  `budget_tokens`. Use the exact provider/model-scoped `output_config.effort`
  branch for both the official and Preview IDs, without sending `thinking`.
- Both [international](https://platform.minimax.io/docs/m-plan/quickstart.md)
  and [China](https://platform.minimaxi.com/docs/m-plan/quickstart.md)
  M Plan quickstarts use the exact official ID with a Subscription Key.
  The [international](https://platform.minimax.io/docs/m-plan/token-plan-notice.md)
  and [China](https://platform.minimaxi.com/docs/m-plan/token-plan-notice.md)
  legacy notices explicitly include M3.1 Flash in both M Plan and Token Plan.
  Register the official ID on each region's default and `m-plan` routes;
  legacy Token Plan uses that subscription route, not a new `token-plan`
  endpoint. Add the missing China default registration with the documented
  `https://api.minimax.cn/anthropic` base and independent `MINIMAX_CN_API_KEY`;
  do not reuse subscription keys or change actual credentials/enabled state. The
  [M Plan lifecycle](https://platform.minimax.io/docs/m-plan/intro.md)
  says Preview is offline and old-name requests are served by the official
  version: preserve its compatibility route, not an independent current offer.
  This supersedes the earlier Preview-only review, not the old model IDs.
- DashScope Token Plan's
  [personal](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview)
  and [team](https://help.aliyun.com/zh/model-studio/token-plan-team-overview)
  lists add exact `deepseek-v4.1-flash-1017` with reasoning and image input.
  Add only the Token Plan route, leaving exact token limits and efforts unknown;
  do not inherit the unversioned model's advisory limits or Coding eligibility.
  [China](https://help.aliyun.com/zh/model-studio/coding-plan) and
  [international](https://www.alibabacloud.com/help/en/model-studio/coding-plan)
  Coding lists still show the same six current IDs; their absence of historical
  IDs is not affirmative removal evidence. Xiaomi's
  [deprecation notice](https://mimo.mi.com/docs/en-US/updates/deprecate) says V2.5
  remains valid until **October 21, 2026 at 10:00 Beijing time**; conflicting
  switching-hour documentation remains unresolved.
  [StepFun's Plan guide](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api.md)
  retains five IDs and router restrictions.
  [Z.AI](https://docs.z.ai/devpack/notice/api-key-management.md) and
  [BigModel](https://docs.bigmodel.cn/cn/coding-plan/latest-model.md) key-type
  billing does not establish universal subscription eligibility.
- The [OpenCode Go protocol table](https://opencode.ai/v2/docs/console/go/)
  and [public IDs](https://opencode.ai/inference/go/v1/models) confirm the
  missing `minimax-m3.1-flash-free` on **Chat**, not MiniMax's Messages route.
  Add that exact ID without copying native MiniMax efforts or limits.
  Its **1,000,000 / 512,000** and vision/reasoning metadata are advisory
  values from models.dev's Go entry, not verified Go service hard limits.
  The current discovery still contains 37 identifiers, not a protocol matrix;
  keep historical routes absent from today's list and defer unsupported
  Gemini/other discovery-only protocols.
- The [Kilo public model interface](https://api.kilo.ai/api/gateway/models)
  confirms missing `kilo-auto/small`: **262,144** context, **32,768** output,
  image/reasoning/tools support. Keep default output unset and effort options
  empty for this dynamic virtual model. Small
  [uses credits](https://kilo.ai/docs/getting-started/using-kilo-for-free);
  it is not Free. Other Auto limits, including Free's 32,768, are unchanged.
  OpenRouter Auto remains 2,000,000 context with unknown output ceiling.
- Rechecked BytePlus Coding, both Volcengine plans and all three registered
  Tencent TokenHub transports against their official protocol tables:
  no additional confirmed registered-route differences. Tencent Responses
  still has 28 IDs and requires JSON handling, complete chronological history
  and no remote compaction. BytePlus `gpt-oss-120b` remains Chat-only.
  Preserve Volcengine's seven historically reviewed dated IDs with their
  unresolved subscription evidence and do not delete models merely marked
  "upcoming offline".
- Tencent [Coding](https://cloud.tencent.com/document/product/1772/128947),
  [personal general/Hy](https://cloud.tencent.com/document/product/1823/130060),
  [enterprise professional](https://cloud.tencent.com/document/product/1823/130659)
  and [light](https://cloud.tencent.com/document/product/1823/131173) remain
  distinct unregistered SKUs, each with per-model protocol/qualification gaps.
  Personal uses `tc-code-latest`; enterprise uses `auto`, and light is
  auto-only. Guangzhou and Singapore have separate hosts, keys and allowlists;
  do not copy Guangzhou's Kimi K2.6/M2.7 eligibility to Singapore.
  The personal/Coding interactive-use restrictions are not enterprise API
  restrictions. Guangzhou retirements on **October 31** and **November 17,
  2026** are still future events, not cross-region removal evidence.

## 2026-10-19 incremental review

- Refreshed models.dev revision
  `6a843075ca9ce62e9a5138e1673b51447c96dd8d4ad352172d293958ee1a99fb`.
  The refresh initially retained all 54 supplement models. Artifact additions
  are `openrouter/unbiased/pareto-26.10` and
  `venice/deepseek-v4-1-flash-1017`; aggregator audit records do not create
  runtime routes. The Venice addition has not been independently confirmed
  against an authenticated endpoint; its upstream limits remain advisory.
- Rechecked the [OpenRouter public list](https://openrouter.ai/api/v1/models)
  at 2026-10-18 20:39 UTC. The new Pareto entry and five changed records
  (`~unbiased/pareto-latest`, `~deepseek/deepseek-flash-latest`,
  `deepseek/deepseek-v4.1-flash`, `z-ai/glm-5.3-flash`, and
  `deepseek/deepseek-v4-pro-0813`) match the advertised context or output
  changes. These are service metadata, not verified inference limits.
- The [Codex lifecycle](https://learn.chatgpt.com/docs/models) still recommends
  GPT-6.1 Sol / GPT-6 Luna where available and documents GPT-6 Astra; the six
  previously removed ChatGPT-sign-in IDs remain retired. Availability still
  depends on the account, plan, client, and workspace administrator.
- Correct Codex-specific defaults using OpenAI's
  [published Codex model metadata at revision 8c076942](https://github.com/openai/codex/blob/8c076942cb70fba8e5c4732c23f1faabc4aed484/codex-rs/models-manager/models.json).
  GPT-6.1 Sol and GPT-6 Luna have a default context of 272,000 tokens, not the
  general API's 1,050,000-token window. Promote these two existing Codex IDs
  from the supplement to reviewed built-ins, retaining the API entries and
  leaving unreported Codex output limits unset. The public metadata also
  specifies 272,000 for the three GPT-5.6 Sol/Terra/Luna defaults, correcting
  their stale 372,000 values. Retain the existing 95% effective-context policy;
  do not treat `max_context_window` as the default context. Local account
  metadata differs for GPT-6.1 Sol (500,000); it is account-scoped evidence,
  not a global default or proof of other accounts' entitlements. No credential
  or inference call was used for this check.
  The same published metadata supports `low/medium/high/xhigh/max` for both
  promoted Codex models and additionally `ultra` for GPT-6.1 Sol only.
  Preserve that distinction without changing general API reasoning options.
  Keep GPT-6 Astra first so promotion does not change the provider's preferred
  default model.
- Rechecked the [DeepSeek Responses guide](https://api-docs.deepseek.com/guides/responses_api/):
  `deepseek-flash` image input and `deepseek-v4-pro`-only server-side search
  remain consistent with the current model-specific routing. The refreshed
  upstream Flash display/description changes do not establish new subscription
  eligibility or justify copying general API limits into reviewed routes.
- The [Kilo public model list](https://api.kilo.ai/api/gateway/models) now
  advertises `kilo-auto/free` output up to 32,768 tokens, replacing 10,000.
  Its 256,000-token context, text-only input, and reasoning/tools remain
  unchanged. Update only the upper bound, leaving default output unset.
  The other three Kilo Auto routes and OpenRouter `auto` metadata are unchanged.
- The [TokenHub protocol matrix](https://cloud.tencent.com/document/product/1823/130079)
  now independently confirms 28 Responses IDs. Register only those reviewed
  IDs on `tencent-tokenhub@responses` at `https://tokenhub.tencentmaas.com/v1`;
  do not copy the Chat/Messages allowlist or infer new model capabilities.
  GLM-5.2 is marked compatibility mode. The
  [Responses guide](https://cloud.tencent.com/document/product/1823/135873)
  and [compatibility guide](https://cloud.tencent.com/document/product/1823/133813)
  require full-history `input` for models without `previous_response_id`
  support; use that conservative boundary for the endpoint, with function
  tools only, no server-side compaction or built-in web search.
  The compatibility guide confirms standard JSON responses as well as SSE.
  Keep JSON transport and send chronological full history with matching
  function `call_id` values, omitting `previous_response_id`.
- Independently rechecked `byteplus@coding`, `volcengine@coding/@plan`,
  `tencent-tokenhub@default/@messages`, and all three OpenCode Go endpoints:
  no other confirmed registered-route differences. BytePlus still confirms
  its own 13 Coding IDs, with GPT-OSS Chat-only; do not copy domestic Volcano
  IDs. Agent Plan's seven dated IDs remain unconfirmed rather than retired.
  OpenCode Go discovery still returns 37 IDs without protocol/capability
  fields; do not infer new static routes from it. Existing subscription
  sources and unresolved exact-ID evidence below remain applicable.
- Refine the Tencent subscription coverage gap into distinct SKUs:
  [personal general and Hy](https://cloud.tencent.com/document/product/1823/130060),
  [enterprise professional](https://cloud.tencent.com/document/product/1823/130659)
  in Guangzhou/Singapore, and
  [enterprise light](https://cloud.tencent.com/document/product/1823/131173)
  in both regions. Hy lists only `hy3`, `hy3-preview`, `hy3-202608`,
  `hy4-preview` without multimodal support; enterprise light lists only
  `auto`, not the professional allowlist. Published protocol URLs do not
  establish model-by-protocol subscription eligibility or Holon authorization.
  Personal automation restrictions must not be applied to enterprise API
  products. Coding Plan still confirms only `tc-code-latest` on Chat/Messages,
  not Responses. The professional Flash-0731 and M2.7 retirement dates are
  October 31 and November 17, 2026 respectively, not removals for this cycle.
  General API Singapore coverage is also still unregistered.
- Independently rechecked all registered China subscription variants:
  `dashscope@coding-plan/@coding-plan-intl/@token-plan`, `xiaomi@token-plan`,
  `stepfun@plan`, and MiniMax international/China M Plan and legacy Token Plan.
  No additional model change was confirmed. The
  [China Coding Plan list](https://help.aliyun.com/zh/model-studio/coding-plan)
  currently lists the same six exact versions as the international list;
  retained `qwen3-max-2026-01-23`, `qwen3-coder-next`, `qwen3-coder-plus`, and
  `glm-4.7` lack current-list eligibility evidence, not affirmative retirement
  evidence. Token Plan eligibility must use the union of
  [personal](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview)
  and [team](https://help.aliyun.com/zh/model-studio/token-plan-team-overview)
  lists. Its `deepseek-v3.2` and undated `deepseek-v4-flash`, plus the six
  historical supplement routes, still lack exact-ID eligibility evidence.
  No historical route was removed on absence alone.
- The [MiniMax lifecycle](https://platform.minimax.io/docs/guides/models-intro.md)
  reaches its **2026-10-19** M2.5/M2.7 subscription migration date in this
  Beijing cycle: requests redirect to M3.1 Flash Preview, not a reason to
  delete old IDs. General API retirement remains **2026-11-18**. Xiaomi's
  announced V2.5 final stop remains **2026-10-21 10:00 Beijing time**.
  Z.AI/BigModel's separate Coding Plan key/alternative protocol gaps remain
  disclosed below; no new `coding-plan` endpoint was inferred or registered.

## 2026-10-18 incremental review

- Refreshed models.dev revision
  `dae7c0c79d38964b123dd0cc8c64a4520d6d61816950f9ca8928bbbeab79dd85`.
  The supplement retains the same 54 models and existing endpoint routes.
  Artifact-only additions are two OpenRouter and three Together records; no
  aggregator runtime routes are automatically added.
- The [OpenRouter public model list](https://openrouter.ai/api/v1/models),
  rechecked at 2026-10-17 20:36 UTC, agrees with all seven added/changed
  OpenRouter records' context and output limits, including the five output-limit
  corrections. The [Together serverless catalog](https://docs.together.ai/docs/serverless/models)
  returned HTTP 403 during this check. Its three additions remain secondary
  models.dev audit data, not independently verified service support or limits.
- Rechecked the [OpenCode Go protocol table](https://opencode.ai/v2/docs/console/go/)
  and [public model list](https://opencode.ai/inference/go/v1/models):
  all currently documented non-Gemini routes are already covered. Do not infer
  retirement of older IDs or route Gemini's native protocol through Chat.
- Rechecked the [Codex model lifecycle](https://learn.chatgpt.com/docs/models).
  The recommended `gpt-6.1-sol`/`gpt-6-luna`, documented `gpt-6-astra`, and six
  retired ChatGPT-sign-in IDs are already covered. Account/client availability
  still requires its own entitlement; API metadata does not grant Codex access.
- MiniMax's [international M Plan](https://platform.minimax.io/docs/m-plan/intro),
  [China M Plan](https://platform.minimax.cn/docs/m-plan/intro), and
  [legacy Token Plan notice](https://platform.minimax.io/docs/m-plan/token-plan-notice.md)
  explicitly cover `MiniMax-M3.1-Flash-Preview`.
  The [Anthropic API reference](https://platform.minimax.io/docs/api-reference/text-anthropic-api.md)
  limits its current eligibility to M Plan / MiniMax Code, states an exact
  1,000,000-token context, image/video input, and five thinking efforts
  (`low`, `medium`, `high`, `xhigh`, `max`; not disabled). Output maximum remains
  unknown: neither the older M3 limit nor example request sizes establish it.
  Keep this model off the ordinary `minimax@default` API route. International
  and China subscription routes require separate, explicit credentials;
  [China quickstart](https://platform.minimax.cn/docs/m-plan/quickstart) confirms
  `api.minimax.cn`, not an inferred regional alias.
  [Model lifecycle](https://platform.minimax.io/docs/guides/models-intro)
  schedules M2.5/M2.7 subscription migration from **2026-10-19** and general API
  retirement on **2026-11-18**. Neither has happened as of this review; retain
  existing models and do not infer every legacy ID's new subscription eligibility.
- [International DashScope Coding Plan](https://www.alibabacloud.com/help/en/model-studio/coding-plan)
  directly documents the `coding-intl.dashscope.aliyuncs.com` Chat/Anthropic bases
  and six exact IDs: `qwen3.7-plus`, `qwen3.6-plus`, `qwen3.5-plus`, `kimi-k2.5`,
  `glm-5`, `MiniMax-M2.5`. The first four explicitly accept images.
  Add only that allowlist on a separately keyed Anthropic endpoint; leave exact
  token limits and effort controls unset, rather than copy China Token Plan
  metadata or its larger allowlist.
  The [international Token Plan team overview](https://www.alibabacloud.com/help/en/model-studio/token-plan-team-overview)
  confirms a separate product but exposes its exact bases only in the account
  console. This remains an unregistered coverage gap; do not guess its host.
- [China Coding Plan](https://help.aliyun.com/zh/model-studio/coding-plan) and
  [Token Plan](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview)
  provided no newly confirmed model or hard-limit changes relative to the
  existing reviewed routes. Previously recorded unsupported-evidence routes
  remain explicitly unresolved, not newly verified. Token Plan's old
  `qwen3.8-max-preview` ID remains compatible; its separate decision interface
  is not a normal chat model.
- [Xiaomi Token Plan integration](https://mimo.mi.com/docs/en-US/tokenplan/integration/opencode)
  confirms existing V2.6 Pro/Flash limits and images. Its Singapore Token Plan
  host is a documented regional coverage gap, not proof that every China route
  is also available there. The [retirement notice](https://mimo.mi.com/docs/en-US/updates/deprecate)
  stops old V2.5 IDs at **2026-10-21 10:00 UTC+8**, so retain them now; no new
  evidence establishes V2.6 Pro Ultraspeed subscription eligibility.
- [Step Plan](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api.md)
  still lists the existing five models, including the separate router.
  [Messages constraints](https://platform.stepfun.com/docs/zh/api-reference/chat/messages-create.md)
  prohibit router images/documents and built-in search and ignore its effort.
  Do not reinterpret `250K` or `1M` prose as newly confirmed exact integers.
- [Z.AI](https://docs.z.ai/devpack/latest-model.md) and
  [BigModel](https://docs.bigmodel.cn/cn/coding-plan/latest-model.md) continue to
  confirm GLM5.3/Flash with existing modality and effort boundaries. Their
  Anthropic bases are already registered; their documented Responses/Chat
  subscription alternatives are not yet separately registered.
  The [Z.AI](https://docs.z.ai/devpack/notice/api-key-management.md) and
  [BigModel](https://docs.bigmodel.cn/cn/coding-plan/notice/api-key-management.md)
  key-classification notices, effective **2026-10-16**, make key type a billing
  boundary. A shared Anthropic URL alone cannot establish subscription
  entitlement or make every default API model subscription-eligible.
- [DeepSeek Responses](https://api-docs.deepseek.com/guides/responses_api/)
  confirms existing Flash image input and V4 Pro-only built-in search. The
  exact model allowlist and official-URL search isolation are already covered;
  unconfirmed Flash token limits remain unset.
- [BytePlus Coding Plan](https://docs.byteplus.com/en/docs/modelark/coding-plan-personal-plan-overview)
  independently confirms reasoning for `glm-5.1`, `kimi-k2.5`, `gpt-oss-120b`;
  correct these conservative capability omissions without inventing effort
  controls or exact limits from its `k` values.
  Its [OpenCode integration](https://docs.byteplus.com/en/docs/modelark/coding-plan-personal-ai-opencode)
  also documents Responses/Messages alternatives, but **GPT-OSS is Chat-only**.
  These alternative protocol endpoints are not yet separately registered;
  never apply the whole Chat allowlist to them. The candidate BytePlus Agent
  Plan page returned navigation without a readable body, so its eligibility,
  bases and model list remain unknown.
- The current [Volcengine Coding overview](https://ark.volcengine.com/region:cn-beijing/docs/coding-plan-personal-plan-overview)
  explicitly supports Kimi K2.7 Code images; enable that capability only on
  `coding`, not on `plan` or the general API.
  The [Agent Plan overview](https://docs.volcengine.com/docs/ark/agent-plan-personal-plan-overview?lang=zh)
  is readable again this cycle, superseding the previous fetch gap.
  Its [own integration guide](https://ark.volcengine.com/region:cn-beijing/docs/agent-plan-personal-opencode)
  independently confirms `glm-latest`, so add the alias to `plan` without
  copying Coding eligibility. Kimi K3 still requires Medium/Large/Max there.
  Both products document Chat/Messages alternatives to the registered Responses
  endpoints, but those alternatives are not yet separately registered.
  Continue treating `k`-unit limits as inexact; example client settings are
  not service hard limits, and the two products' keys are not interchangeable.
- Tencent [Coding](https://cloud.tencent.com/document/product/1772/128947),
  [personal Token Plan](https://cloud.tencent.com/document/product/1823/130060),
  and [enterprise regional Token Plans](https://cloud.tencent.com/document/product/1823/130659)
  are separate coverage gaps: the current TokenHub default/messages endpoints
  are **not** any of these subscription routes.
  Coding currently explicitly lists `tc-code-latest`, no native multimodal
  support, and Chat/Messages, not confirmed Responses.
  Personal Token Plan uses lkeap for Chat/Messages and tokenhub for Responses.
  Enterprise Guangzhou/Singapore have distinct hosts and model allowlists;
  enterprise `auto` must not be confused with personal `tc-code-latest`.
  Precise per-model subscription limits and protocol compatibility are not
  established by the general API matrix, so do not auto-register those routes
  from models.dev or generic TokenHub metadata.
  Future retirements (Guangzhou DeepSeek Flash-0731 **2026-10-31**, MiniMax M2.7
  **2026-11-17**) have not happened yet and are not cross-region facts.
  Personal plan terms restrict scripts/self-hosted backends/noninteractive
  batch use: protocol compatibility is not permission for arbitrary Holon
  headless automation. Account/tier/usage eligibility remains the user's
  responsibility; no paid or credential-based availability checks were made.

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
