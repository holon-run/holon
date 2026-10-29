# Endpoint-specific catalog review

Initial review: **2026-10-16 Beijing time**; subsequent cycles are recorded below. This is an
evidence record, not a claim that every account, region, subscription tier, or
model was exercised. No credentials were read and no paid inference was called.

The general API catalog is not a subscription allowlist. Preserve previously
reviewed routes unless there is affirmative removal evidence. New routes require
their own endpoint evidence; missing capability flags are conservative omissions,
not proof that a model lacks the feature.

## 2026-10-30 incremental review

This cycle starts from main `330d105b`. Public evidence was sampled on
October 29 UTC (October 30 in Beijing). The frozen upstream revision is
`94aeb98e4f87dfa5df3cdfdee97b54647e697cc40564693badeae1a8c876075c`.
The initial refresh, validation and 662-route metadata gate passed. The
55 supplement models and 16 explicitly reviewed non-default routes did not
change. Final post-change verification is recorded below.

### Confirmed endpoint changes

- Qianfan's independent [retirement table](https://cloud.baidu.com/doc/qianfan/s/zmh4stou3)
  explicitly ends V1 and V2 access, including the registered V2 Chat URL.
  Exact `ernie-5.0`, `ernie-5.0-thinking-latest` and
  `ernie-5.0-thinking-preview` retired on **2026-10-29**; `ernie-x1.1`,
  `deepseek-v3.2` and `deepseek-v3.2-think` retired on **2026-09-29**.
  Both dates have passed in the service's Beijing timezone. Remove only
  those six IDs. `ernie-5.0-thinking-exp` is not explicitly named: preserve
  it without claiming new eligibility. Qianfan has no upstream mapping or
  supplement admission; keep that existing exclusion and test that previous
  supplement entries cannot revive these IDs. Removing the retired leading
  entries naturally selects the existing `deepseek-v4.1-flash-1017` under
  the unchanged default-candidate gate, not a user-config edit. `ernie-5.1`
  remains registered but has neither confirmed context nor candidate
  capabilities; do not invent either to force its selection.
- Volcengine's [ordinary model list](https://ark.volcengine.com/region:cn-beijing/docs/model-list.md)
  now agrees with the [K3.1 model card](https://ark.volcengine.com/models/kimi-k3-1-pro.md)
  on the maximum output. Add **1,048,576** only to
  `volcengine@default/kimi-k3-1-pro`. The model list still states a 4k
  default while the card states 131,072: leave the output default unknown.
  Keep the existing exact context, vision and native low/high/max efforts.
  Coding/Agent Plan pages use abbreviated 1024k output figures, not
  independently confirmed exact integers; do not copy the ordinary ceiling
  into `kimi-k3.1-pro` subscription routes.

### Preserved boundaries

Native DeepSeek's [Responses reference](https://api-docs.deepseek.com/guides/responses_api/)
confirms Flash `input_image` support. The registration and regression already
cover it; this is not a new implementation change. Preserve the reviewed
1,000,000 / 384,000 native ceilings and protocol-specific effort support.
The official Codex model metadata is byte-identical to the preceding cycle;
no login route or limit is inferred from generic OpenAI availability.
Mistral's single `deepseek-v4-1-flash-1017` candidate remains gated pending
endpoint-specific exact limit and effort evidence.

The four changed OpenRouter artifact records match the separately sampled
public listing: DeepSeek V3.1 output 147,456; V4 Pro output 393,216; V4.1 Flash
output 943,718; Nemotron 3 Super context 1,048,576 / output 262,144. These are
audit projections, not automatic builtin updates or proof of an output
default. The artifact also drops Solar Pro 3 and Venice Inkling; upstream
membership changes do not establish native retirement or remove static
routes. Artifact aliases, routes and preferred mappings are unchanged.

### Second endpoint group

Independently checked all 15 registered endpoints: MiniMax international
and China ordinary/M Plan, international Moonshot, BytePlus ordinary/Coding,
StepFun ordinary/Plan, all three Go protocols, Kilo, Together and Fireworks.
The ordinary BytePlus endpoint still has no static routes. No new
implementable route deletion or exact capacity change was confirmed.

- Go's [public listing](https://opencode.ai/inference/go/v1/models) fell
  from 30 to 28 IDs; `mimo-v2.5` and `mimo-v2.5-pro` disappeared while the
  [subscription documentation](https://opencode.ai/v2/docs/console/go/)
  still lists them. All 28 current IDs are covered by each protocol.
  Static Chat/Messages/Responses counts remain 38/34/30 (42 unique IDs);
  listing absences are 10/6/2 respectively, or 14 unique historical IDs
  across the protocols. Absence and conflicting sources do not prove EOS.
- Fireworks' [Qwen3.8-specific Chat section](https://docs.fireworks.ai/api-reference/post-chatcompletions.md)
  confirms the two models' effort controls. The two model IDs and
  `accounts/fireworks/routers/qwen-max-latest` exist only in raw upstream,
  not in the builtin/effective catalog: no static effort field was fixed.
  The [large model](https://fireworks.ai/models/fireworks/qwen3p8-2p4t-a95b)
  is on-demand-only; [Max](https://fireworks.ai/models/fireworks/qwen3p8-max)
  has public serverless eligibility but lacks an independently confirmed
  exact context limit. Model-specific controls do not certify the router.
- BytePlus's decoded SSR [Coding table](https://docs.byteplus.com/en/docs/modelark/coding-plan-personal-plan-overview)
  includes display-name counterparts of all 13 historical entries; that
  does not newly certify six unresolved exact wire IDs or account access.
  Its [retirement table](https://docs.byteplus.com/en/docs/modelark/coding-plan-personal-model-deprecation)
  has a `09.23` date without a year and conflicts with the support table.
  Preserve that uncertainty rather than guessing a deadline or deleting.
- MiniMax international existing-user access through November 18,
  Moonshot's October 31 new-user restriction and BytePlus's November 11
  ordinary batch are not all-user retirements effective in this cycle.
  Kilo's five exact fields, Together's serverless/dedicated boundary and
  StepFun's independent Plan router retain their previous reviewed values.

### Final verification

- Regenerated the builtin snapshot, reran the pinned refresh and validation,
  and verified snapshot/source consistency.
- The 656-route metadata gate passes. Exact review permits only six Qianfan
  retirements and the ordinary K3.1 output ceiling; the other 655 surviving
  field projections, all supplement entries and other preferred mappings
  are unchanged. Qianfan's existing eligible Flash route is the sole
  preferred-model/route replacement.
- Final locked tests pass: catalog 193 passed / 1 ignored; adapter 11,
  coverage 11, reviewed capacity 1, route cache 17 and Responses 77 passed
  (310 passed / 1 ignored total). Two stale Qianfan test assertions were
  corrected without changing the frozen catalog or field-review artifacts.
- `cargo fmt --all -- --check`, warning-free locked all-targets checking
  (`RUSTFLAGS="-D warnings" cargo check --all-targets --locked`) and
  `git diff --check` pass.
- Independent final review found no unresolved confirmed P1/P2 defects.
  Offline verification does not certify live account, region, subscription
  or session eligibility; no paid inference or deployment was performed.

## 2026-10-29 incremental review

This cycle starts from main `20f5e26c`. Public evidence was sampled on
October 28 UTC (October 29 in Beijing). The frozen upstream revision is
`4adab25c2649c5b5dcc43e983d4891125163d558cbbc5db00664facf5629bdc3`.
The generic artifact has three additions, twelve removals, and eleven changed
records. Artifact membership is not endpoint eligibility or native retirement.
In particular, OpenRouter's removed `gpt-5.2-chat` still appears in its public
listing; preserve that distinction.

The new `mistral/deepseek-v4-1-flash-1017` draft is not admitted this cycle.
Its [official native model page](https://docs.mistral.ai/models/deepseek-v4-flash-1017)
confirms the precise API ID, text and function calling, but does not resolve
the exact upstream limits or effort enum. Keep it as a snapshot/artifact
candidate with `endpoint_review_required`, including if an unreviewed local
refresh had already drafted it. Preserve all 55 baseline supplement models
and existing Mistral routes; no unsupported or retirement claim is made.

### Affirmative endpoint retirement evidence

- Volcengine's separate [Coding Plan notice](https://ark.volcengine.com/region:cn-beijing/docs/coding-plan-personal-model-deprecation.md)
  and [Agent Plan notice](https://ark.volcengine.com/region:cn-beijing/docs/agent-plan-personal-model-deprecation-announcement.md)
  ended exact `kimi-k2.7-code` service on **2026-10-28 14:00 UTC+8**.
  Remove only `volcengine@coding/kimi-k2.7-code` and
  `volcengine@plan/kimi-k2.7-code`, including their retained-supplement path.
  Ordinary API and other providers are independent. Preserve `kimi-latest`:
  its documented old target is stale, not affirmative alias retirement.
  `minimax-m3` has not reached its November 2 final cutoff.
- Fireworks' [K2.6](https://fireworks.ai/models/fireworks/kimi-k2p6),
  [K2.7 Code](https://fireworks.ai/models/fireworks/kimi-k2p7-code), and
  [GLM 5.2](https://fireworks.ai/models/fireworks/glm-5p2) cards explicitly
  ended **serverless only** on **2026-09-25**; on-demand continues.
  The affected public IDs are `accounts/fireworks/models/kimi-k2p6`,
  `accounts/fireworks/models/kimi-k2p7-code`, and
  `accounts/fireworks/models/glm-5p2` at `fireworks@default`.
  Do not infer native Moonshot/GLM retirement, revoke user deployment IDs,
  or copy these models' capacities onto custom deployments.
  The retired K2.6 ID was also the built-in provider preference. Prefer the
  existing MiniMax M3 entry, whose current official index explicitly supports
  serverless, rather than falling through to a deployment-only model.
  M3 metadata is unchanged; this is a built-in fallback change, not a rewrite
  of user defaults or account configuration.

### Independent registered-endpoint rechecks

The two evidence groups cover 30 registered endpoints, including
`byteplus@default` with zero static effective routes. The following are
preservation decisions, not guarantees for every account or region.

| Endpoints | Current evidence and retained boundary |
| --- | --- |
| `dashscope@default`, `@token-plan`, `@coding-plan`, `@coding-plan-intl` | Separate Messages and domestic/international plan documents reviewed. No new confirmed field changes. Historical list omissions do not retire routes; quotas are not token capacities. |
| `volcengine@default`, `@coding`, `@plan` | Exact subscription retirement above. Ordinary API EOS filtering and K3.1 `low/high/max` remain; API/plan IDs and capacity conflicts stay separate. |
| `tencent-tokenhub@default`, `@messages`, `@responses` | Each protocol's current schema and eligibility reviewed independently; existing output, reasoning, vision and cache boundaries retained. |
| `qianfan@default` | Current model/schema evidence preserves existing precise IDs; output-limit conflicts remain unresolved, not enlarged. |
| `bigmodel@default`, `zai@default` | Domestic and international Messages/DevPack evidence independently checked; GLM 5.3 text versus Flash vision, separate keys, and unknown limits retained. No new plan endpoint inferred. |
| `xiaomi@default`, `@token-plan` | Current Responses and plan evidence reviewed separately. Existing multimodal/output facts remain; historical ultraspeed plan eligibility is not recertified or inferred from ordinary API. |
| `minimax@default`, `minimax-cn@default`, `minimax@m-plan`, `minimax-cn@m-plan` | Four independent Messages/plan boundaries. International M2.5/M2.7 existing-user access lasts to November 18; plan forwarding aliases are not native-service retirement. Recommended output is not a service default. |
| `moonshot@default` | International K3/K3.1 facts unchanged. K2 new-user restriction on October 31 is not immediate retirement for existing users; planned end-of-December retirement has no precise cutoff yet. |
| `byteplus@default`, `@coding` | Ordinary Singapore API and plan checked separately. Thirteen historical plan routes retained, but current public docs defer to the console/region and do not recertify all thirteen IDs. Missing public IDs are not affirmative EOS. |
| `stepfun@default`, `@plan` | Existing four models and plan router retained. Router text-only limits and unknown exact capacities stay isolated; no new Messages registration inferred. |
| `opencode-go@default`, `@messages`, `@responses` | Current public discovery lists 30 IDs, all already covered. Eight historical IDs missing from discovery are not proven retired. Preserve prior translated routes, native preferred routes, conservative capabilities, and session-header requirements. No real session tested. |
| `kilocode@default` | All five exact static context/output-ceiling/image facts match the public gateway JSON; output request parameters do not establish service defaults. |
| `together@default` | Existing M3 reasoning correction retained. Dedicated deployment specifications are not serverless capacities; the alternate example hostname does not retire the registered host. |
| `fireworks@default` | The three dated serverless retirements above are affirmative. Other index/card deployment flags, Qwen availability and M3 vision remain unresolved; absence or a contradictory flag is not a provider-wide ban. Rune does not generate text and is not an agent-turn addition. |

The current [Go public listing](https://opencode.ai/inference/go/v1/models)
omits eight retained IDs: `cat-jam-free`, `deepseek-v4-flash`, `glm-5.1`,
`kimi-k2.6`, `longcat-2.5-preview-free`, `minimax-m3.1-flash-free`,
`space-bunny`, and `step-5-preview-free`. All 30 currently listed IDs already
have the reviewed protocol coverage. Zen's separate October 26 retirement
notice is not evidence to remove a Go route.

BytePlus's current [plan overview](https://docs.byteplus.com/en/docs/modelark/coding-plan-personal-plan-overview)
does not independently recertify six historical exact IDs:
`bytedance-seed-code`, `dola-seed-2.0-code`, `dola-seed-2.0-lite`,
`dola-seed-2.0-pro`, `glm-5.1`, and `kimi-k2.5`. Keep these as explicit
evidence gaps, not newly certified routes or proven retirements. Do not
borrow ordinary API or mainland China capacities.

Codex's pinned catalog remains byte-identical to the previous cycle
(`126a0ee92ed2c5e73cd8a8f7e37563e8527552d7`); do not add new API-to-Codex
routes without endpoint evidence. DeepSeek Pricing/Responses preserve the
old-name forwarding boundary, reviewed Responses ceilings and native-unit
isolation. The separate [coverage-enumeration issue #3788](https://github.com/holon-run/holon/issues/3788)
remains unresolved: a field-gate pass must not be described as proof that all
compatible routes are enumerated.

Final review removed only the five retired routes from the metadata baseline.
All 661 surviving raw/effective/provenance/status projections remain unchanged,
as do the 55 supplement models and their 16 explicit routes. Other provider
defaults and surviving model preferences are unchanged; Fireworks alone moves
its built-in preference from retired K2.6 to the existing MiniMax M3 entry.

Local verification passed: pinned refresh, catalog validation, 661-route
coverage gate, retirement and cross-endpoint regressions, and the exact Mistral
new/retained/peer admission tests. The final serial run passed 173 catalog
tests (one ignored), 33 discovery tests, 19 models.dev integration tests,
17 route-cache tests and 54 Responses tests, followed by formatting,
warning-free `cargo check --locked --all-targets`, and `git diff --check`.
The exact reasoning-contract regression also passed: the replacement Coding
and Plan K3.1 Pro routes preserve route-existence, reasoning and empty-effort
assertions; M3 retains its reviewed low/medium/high effort enum.

Independent frozen-artifact review and the subsequent test-only recheck found
no confirmed P1/P2 blockers. No live account/session or paid-inference
acceptance was performed. These local gates do not resolve the compatible-route
enumeration gap tracked in #3788.

## 2026-10-28 incremental review

This cycle starts from main `6a848009`; public endpoint evidence was checked
on October 27 at 20:36–20:43 UTC (October 28 in Beijing).
Refresh uses upstream revision
`0bc5ae4e21918c0396a4885bd6b427e7a5af4c60eddf65795fc05ad0f2f8f7bb`.
The artifact has three additions, two removals, and nine changed records;
the supplement's 55 models and explicit route associations are unchanged.
The eleven added/changed OpenRouter records match its independently fetched
[public listing](https://openrouter.ai/api/v1/models); the two removed records
are absent there, not proven retired on their native providers.

### Confirmed endpoint changes

- [OpenAI's Codex catalog](https://github.com/openai/codex/blob/126a0ee92ed2c5e73cd8a8f7e37563e8527552d7/codex-rs/models-manager/models.json)
  now includes `gpt-6.1-terra`. Its default context is 500,000, distinct
  from the API's 1,050,000 and optional Codex maximum 872,000.
  Explicit admission must preserve the future-model review gate, existing
  preferred model, unknown output capacities, and independent runtime budget.
  Text/image input, parallel tools, shell execution, and six effort levels
  are confirmed; image generation is not inferred.
- [Go's model table](https://opencode.ai/v2/docs/console/go/) describes
  native formats, not exclusive access. Its
  [harness guide](https://opencode.ai/v2/docs/console/harnesses/) confirms
  translation across all three existing APIs for current text models.
  Review all 30 exact table IDs, including `gemini-4-argon`, in both the
  static catalog and discovery. Preserve native preferred routes and metadata;
  new translated routes require conservative field narrowing, not native
  capability or capacity inheritance. Historical names are not renamed,
  retired, or automatically expanded. Stable `x-opencode-session` is an
  integration prerequisite, not a newly verified transport implementation.

### Implemented metadata and preservation checks

The regenerated builtin snapshot adds two models and 62 routes: one explicit
Codex admission and 61 Go routes, completing 30 current IDs across the three
registered protocols. The 60 non-preferred Go routes explicitly clear nullable
capacities and verbosity, disable unconfirmed capabilities and cache controls,
and retain empty effort options. Gemini's preferred text fallback is also
conservative because its native Google protocol is not registered.

All 384 previous builtin models, 533 routes, 170 aliases, 43 provider defaults,
and 384 per-model preferred routes are unchanged. The offline field review adds
62 records to the baseline for **666 total**, with all **604 existing field
projections unchanged**. New output capacities remain unknown; the existing
8192-token runtime fallback is policy, not a certified output limit.
Real snapshot generation, Terra admission, 11 Go-related regressions and
snapshot consistency passed. Final serial verification completed on October 27
at 21:16 UTC: pinned refresh, `models-dev validate`, and the 666-record
`models-dev coverage --check` passed; catalog tests passed 171 with one ignored,
discovery 33, models.dev integration 19, route cache 17, and Responses 53.
`cargo fmt --all -- --check`, warning-free `cargo check --locked --all-targets`,
and `git diff --check` passed. Independent review found no confirmed P1/P2
blockers and separately checked all 62 additions and unchanged baseline objects.
These local gates do not establish live Go session or paid-inference acceptance.

### Independent endpoint rechecks

The source links below refer to the same independently fetched endpoint
documents as the preceding cycle unless a new conflict is noted.

| Registered endpoints | This cycle's result and preserved boundary |
| --- | --- |
| `dashscope@default` | Messages matrix unchanged; 46 existing exact IDs covered, with model-specific capability exceptions. |
| `dashscope@token-plan` | Personal/team documentation unchanged; six historically unconfirmed routes are not newly certified. |
| `dashscope@coding-plan`, `@coding-plan-intl` | Domestic/international tables independently unchanged; no cross-plan eligibility inference. |
| `bigmodel@default`, `zai@default` | China and international model/key/cache sources independently unchanged; moved documentation is not endpoint retirement. |
| `minimax@default`, `minimax-cn@default` | Separate regional schemas unchanged; implicit cache is not explicit control, recommended output is not default capacity. Existing native M2.5/M2.7 remain until November 18. |
| `minimax@m-plan`, `minimax-cn@m-plan` | Independent plan tables unchanged; Preview/old plan names forward to Flash, without borrowing native retirement or regional quotas. |
| `volcengine@default` | Native matrix and EOS unchanged. New K3.1 thinking/sampling conflicts are documented in the [capacity evidence](metadata-evidence.md); retain `low/high/max` and unknown output capacities. |
| `volcengine@coding`, `@plan` | Independently unchanged. K2.7 Code EOS is **October 28 at 14:00 UTC+8**, later than this 04:30 review; retain it now. M3 remains until November 2 at 14:00 UTC+8. |
| `tencent-tokenhub@default`, `@messages`, `@responses` | Protocol matrix and three schemas unchanged; Chat's reviewed fields do not migrate to the other protocols. |
| `qianfan@default` | Model list/Chat schema unchanged; preserve conflicting capacities and historical IDs without affirmative retirement evidence. |
| `moonshot@default` | International model/Chat/pricing/cache text unchanged. October 31 restricts new K2 users; full retirement remains planned for end of December, with no exact cutoff invented. |
| `byteplus@coding` | Independently unchanged; retain 13 names and Singapore subscription boundary without borrowing ordinary API limits. |
| `stepfun@default`, `@plan` | Separate model/EOS/plan documents unchanged; keep five plan IDs, router restrictions, and non-integer capacity gaps. |
| `opencode-go@default`, `@messages`, `@responses` | Confirmed translated-access change above; current table is 30 IDs, public discovery 38, and absence of historical names is not retirement. |
| `kilocode@default` | Five `kilo-auto` objects unchanged. Two new gateway discovery IDs are already parseable; OpenRouter records do not imply Kilo static admission. |
| `together@default` | Six model cards unchanged; `.ai` examples do not retire registered `.xyz`, and Dedicated limits do not overwrite serverless fields. |
| `fireworks@default` | Relevant model cards/cache guide unchanged. Rune's current card says it returns probabilities rather than generated text, so do not add it as an agent-turn model despite index marketing changes. |
| `openrouter@default` Auto | Auto metadata unchanged; routing cost tiers and underlying model capacities are not guaranteed Auto effort levels or limits. |
| DeepSeek native endpoints | Pricing and Responses documentation rechecked; no new qualifications or exact integer capacities established by shorthand. |

This is public metadata review, not live account, inference, or session
acceptance. No credentials, paid probes, deployment, or persistent user
configuration changes are part of this cycle.

## 2026-10-27 incremental review

This cycle starts from main `95236be5`. Independent endpoint checks were made
on 2026-10-26 at 21:22–21:33 UTC (October 27 in Beijing), in addition to the
ordinary API refresh. No credentials, paid inference, deployment, or account
availability checks were used.

### Ordinary API changes and endpoint admission

- The refreshed upstream revision is
  `075da82620b738f6da30c52e4e2cb4ff19a054dcaef9018519c233a71bc5f3f8`.
  The artifact has eight additions, six removals, and four changed records.
  Changed OpenRouter output limits match the separately saved public model
  listing; aggregator data does not grant native or subscription access.
- The [Terra API model card](https://developers.openai.com/api/docs/models/gpt-6.1-terra)
  confirms context 1,050,000, output ceiling 128,000, text/image input, and six
  reasoning levels. Tool calling requires Responses, already used by
  `openai@default`. The public [Codex model metadata](https://learn.chatgpt.com/docs/models)
  does not list Terra. New OpenAI-to-Codex aliases now require endpoint review,
  while previously reviewed aliases remain admitted subject to existing
  retirement/modality filters. Missing Codex evidence is not retirement.
- DeepSeek's [pricing](https://api-docs.deepseek.com/quick_start/pricing/) and
  [Responses guide](https://api-docs.deepseek.com/guides/responses_api/) do not
  establish new subscription qualifications or integer limits this cycle.

### Volcengine ordinary Responses

The [native retirement notice](https://ark.volcengine.com/region:cn-beijing/docs/model-deprecation-notice.md)
establishes these already effective EOS dates, all at 14:00 UTC+8:

| Exact ordinary API ID | EOS date |
| --- | --- |
| `deepseek-v3-2-251201` | 2026-07-30 (batch-specific exception) |
| `doubao-seed-1-8-251228` | 2026-09-21 |
| `doubao-seed-1-6-251015` | 2026-09-21 |
| `doubao-seed-1-6-flash-250828` | 2026-09-21 |
| `doubao-seed-1-6-vision-250815` | 2026-09-21 |
| `doubao-seed-2-0-lite-260215` | 2026-10-22 (batch-specific exception) |
| `doubao-seed-2-0-mini-260215` | 2026-10-22 (batch-specific exception) |

Remove ordinary registrations where present and prevent supplement refresh
from resurrecting these default API routes. This is not a global `ModelRef`
ban: independently registered coding/plan and hosted offerings remain.
EOS may automatically migrate an old access point rather than make every
request fail. Pro/code-preview `260215` do not reach EOS until November 24.
Removing the retired Seed 1.8 ordinary default necessarily changes only that
provider's default candidate; new registrations must not reorder other defaults.

The [native model list](https://ark.volcengine.com/region:cn-beijing/docs/model-list.md),
[Responses thinking matrix](https://ark.volcengine.com/region:cn-beijing/docs/responses-api-deep-thinking.md),
and [Responses schema](https://ark.volcengine.com/region:cn-beijing/docs/create-model-responses-api.md)
confirm five missing exact Responses IDs:
`doubao-seed-2-1-pro-260915`, `doubao-seed-2-1-lite-260915`,
`deepseek-v4-1-flash-261017`, `deepseek-v4-1-flash-260910`, and
`deepseek-v4-1-flash-economy-261017`. They support reasoning and image input.
Use real effort levels, not compatibility aliases: Seed `minimal/low/medium/high`,
DeepSeek `none/low/high/max`. Capacity expressions such as `1024k`, `384k`,
and `128k` do not establish new exact integers.

The [native K3.1 card](https://ark.volcengine.com/models/kimi-k3-1-pro.md)
separately confirms the direct API ID `kimi-k3-1-pro`, distinct from the
dotted subscription name and the dated internal response ID. Responses,
image input, always-on reasoning, `low/high/max`, and context 1,048,576 are
confirmed. Its exact output integers conflict with the overview; output
default/ceiling remain unknown. Native implicit caching or Responses
`caching`/`previous_response_id` does not imply Anthropic `cache_control`.

### Registered endpoint rechecks

Each endpoint below was checked independently; grouped rows do not imply
shared credentials, regions, protocols, capacities, or model entitlements.

| Registered endpoints | Evidence and retained boundary |
| --- | --- |
| `dashscope@default` | [Messages matrix](https://help.aliyun.com/zh/model-studio/anthropic-api-messages): all 46 listed exact IDs covered; existing host remains supported. Model-specific effort/cache exceptions remain. |
| `dashscope@token-plan` | [Personal](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview), [team](https://help.aliyun.com/zh/model-studio/token-plan-team-overview), and [team quickstart](https://help.aliyun.com/zh/model-studio/token-plan-team-quickstart) unchanged; team shared Beijing hosts are published, but deployment/account details remain independent. Six historically unconfirmed routes are not newly certified. |
| `dashscope@coding-plan`, `@coding-plan-intl` | [Domestic](https://help.aliyun.com/zh/model-studio/coding-plan) and [international](https://www.alibabacloud.com/help/en/model-studio/coding-plan) tables independently unchanged; historical absence is not retirement. |
| `volcengine@coding`, `@plan` | [Coding](https://ark.volcengine.com/region:cn-beijing/docs/coding-plan-personal-plan-overview.md) and [Agent](https://ark.volcengine.com/region:cn-beijing/docs/agent-plan-personal-plan-overview.md) unchanged. Their independent notices retain K2.7 Code until October 28 14:00 UTC+8 and M3 until November 2 14:00 UTC+8. The October 25 K3 discount expiry is not retirement. |
| `tencent-tokenhub@default`, `@messages`, `@responses` | [Protocol matrix](https://cloud.tencent.com/document/product/1823/130079) and three schemas unchanged; yesterday's exact Chat capacity/reasoning/vision repairs do not extend to other protocols. Guangzhou/Singapore keys remain distinct. |
| `qianfan@default` | [Models](https://cloud.baidu.com/doc/qianfan/s/rmh4stp0j) and [Chat schema](https://cloud.baidu.com/doc/qianfan-api/s/3m7of64lb) unchanged; yesterday's 15 additions and capacity-conflict cleanup remain. Absence is not ERNIE X1.1/DS V3.2 retirement. |
| `bigmodel@default`, `zai@default` | [China latest models](https://docs.bigmodel.cn/cn/coding-plan/latest-model.md) and [international DevPack](https://docs.z.ai/devpack/latest-model.md), key and model documents independently checked; no new Messages/plan ID confirmed. |
| `minimax@default`, `minimax-cn@default` | Independent [international](https://platform.minimax.io/docs/api-reference/text-chat-anthropic.md) and [China](https://platform.minimax.cn/docs/api-reference/text-chat-anthropic.md) schemas retain M3/M3.1 limits and distinct effort support. Recommended output is not a server default. Implicit cache exists despite unsupported explicit `cache_control`. M2.5/M2.7 remain available to existing native API users until November 18, not immediately retired. |
| `minimax@m-plan`, `minimax-cn@m-plan` | Independent [international](https://platform.minimax.io/docs/m-plan/intro.md) and [China](https://platform.minimax.cn/docs/m-plan/intro.md) tables retain M3.1 Flash qualifications and unknown Preview limits; old M2.5/M2.7 plan names forward to M3.1 rather than follow native retirement. |
| `moonshot@default` | [Models](https://platform.kimi.ai/docs/models.md), [Chat](https://platform.kimi.ai/docs/api/chat.md), and [pricing](https://platform.kimi.ai/docs/pricing/chat.md) unchanged. K2 new-user restriction starts October 31; full retirement is planned for **end of December 2026**, not November. Kimi Code remains a separate unregistered product. |
| `byteplus@coding` | [Quickstart](https://docs.byteplus.com/en/docs/modelark/coding-plan-personal-get-started) and [plan](https://docs.byteplus.com/en/docs/modelark/coding-plan-personal-plan-overview) cover the existing 13 names; SSR content was checked. Ordinary `/api/v3` is separately billed; router capabilities and integer shorthand remain unexpanded. |
| `stepfun@default`, `@plan` | [Models](https://platform.stepfun.com/docs/zh/guides/models/list.md), [retirement](https://platform.stepfun.com/docs/zh/guides/models/deprecations.md), and [plan integration](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api.md) retain separate five-ID plan access and router restrictions. `INF`/`250K` are not integer defaults/ceilings; unregistered Messages is not added. |
| `opencode-go@default`, `@messages`, `@responses` | [Go protocol table](https://opencode.ai/v2/docs/console/go/) and public discovery unchanged; K3.1/Iceman remain Chat-only. Zen is separate; its delayed DeepSeek retirement publication does not remove Go routes. |
| `kilocode@default` | [Gateway listing](https://api.kilo.ai/api/gateway/models) and [auto model guide](https://kilo.ai/docs/code-with-ai/agents/auto-model) retain the same five exact records. `max_completion_tokens` is not a new server default; reasoning support is not an effort enum. |
| `together@default` | [M3](https://www.together.ai/models/minimax-m3) and other checked cards retain yesterday's repair. Examples now recommend `api.together.ai`, but this is not evidence that the registered `.xyz` host retired. Dedicated M2.7 limits do not overwrite serverless limits; blocked documentation remains an evidence gap. |
| `fireworks@default` | [329-ID index](https://fireworks.ai/models), model cards, and [prompt caching](https://docs.fireworks.ai/guides/prompt-caching.md) retain yesterday's M3 ceiling fix. Service default 2048 does not overwrite the runtime budget; the M3 vision conflict remains unresolved. Prefix caching is not Anthropic control support. |

No new protocol, endpoint, credential, default/fallback configuration, or
production service is enabled by these checks.

### Final field review and local verification

- The reviewed metadata baseline contains 604 routes: one ordinary OpenAI API
  addition, six native Responses additions, three ordinary EOS removals, and
  597 unchanged existing
  raw/effective/evidence projections. The Lite coding/plan raw image-input
  capability remains at its previous value; effective plan capabilities
  are covered separately.
- The artifact (1,087 models), supplement (55 models), mapping audit (zero
  stale mappings), and `models-dev coverage --check --json` pass.
- `cargo test --locked --lib model_catalog`: 186 passed, one ignored.
  The three `models_dev_*` integration targets pass 11, seven, and one tests;
  `route_cache` passes 17 tests and `responses` passes 76.
- `cargo fmt --all -- --check`,
  `RUSTFLAGS="-D warnings" cargo check --locked --all-targets`, and
  `git diff --check` pass. The preceding 17-test Volcengine run also passes;
  new Codex admission and ordinary EOS filtering have failing-before /
  passing-after regression evidence.
- Independent final review of the frozen increment found no confirmed P1/P2
  blockers or outstanding facts blocking its conclusion. It covered Codex
  admission, ordinary EOS versus plan isolation, native Responses wiring,
  and exact preservation of the 597 existing metadata records. Local checks
  and static review do not establish account availability or live supplier
  behavior.

## 2026-10-26 incremental review

This cycle starts from main `31e3ffcd`, including #3746's field-review gate
and independent runtime-budget fallback. Preserve reviewed endpoint limits,
cache defaults, and user overrides; an upstream candidate is not an accepted
runtime field or subscription entitlement.

- Refresh revision:
  `f72eac48dfa8e32794722e77de0c66f2cf6ae79ec2026436cd524394eb41b83a`.
  The artifact contains 1,084 records: three additions, no removals, and
  two changed records. The new IDs are `chutes/Qwen/Qwen3.8-Max-0902`,
  `chutes/deepseek-ai/DeepSeek-V4-Pro-0813`, and
  `openrouter/liquid/lfm-2.5-2.6b`. Chutes capacities remain unknown;
  these `remote_discovered` artifact entries are not newly accepted runtime
  routes. All 54 supplement models and 16 explicit routes retain their
  previous semantics.
- Compared the three added/changed OpenRouter records with this cycle's
  467-entry [public model list](https://openrouter.ai/api/v1/models).
  LFM2.5-2.6B has context 128,000 and output 8,192;
  `meta-llama/llama-3.2-3b-instruct` retains context 131,072 and changes
  output 72,000 → 117,964; `qwen/qwen3.6-27b` retains context 262,144
  and changes output 65,536 → 81,920. Each matches the public
  `top_provider.max_completion_tokens`. This is a changed-record comparison,
  not a full-catalog reconciliation or proof of subscription access.
- Re-fetched [Codex model metadata](https://github.com/openai/codex/blob/main/codex-rs/models-manager/models.json):
  SHA-256 remains
  `943ca7fe1d19ed019054158303f0dbbd80b3bef43b3aa425a71aa7cb3fb52c2b`,
  byte-identical to the previous cycle. The seven listed, non-retired models
  remain covered. Keep the published 272,000 default context separate from
  the optional/account-specific 872,000 maximum. The
  [lifecycle page](https://learn.chatgpt.com/docs/models) still distinguishes
  ChatGPT sign-in retirements from API access.
- Rechecked DeepSeek [pricing](https://api-docs.deepseek.com/quick_start/pricing/)
  and [Responses restrictions](https://api-docs.deepseek.com/guides/responses_api/).
  The existing aliases, published capacities, Flash image support, and
  Pro-only server-search boundary remain unchanged. Preserve #3746's
  distinction between native exact integers and separately reviewed Responses
  ceilings; do not derive endpoint-specific limits or cache policies from
  native API metadata alone.
- The refresh-only stage passed artifact/supplement validation and
  `coverage --check`: all 585 runtime routes retained their protected facts
  and inputs, so that stage did not rewrite `metadata-review.json`.
  Its 148 unknown contexts and 205 unknown output ceilings were existing
  review debt, not newly completed reviews. The subsequent official
  endpoint review found the separate builtin repairs below; their changed
  inputs require a new review and final verification.

### Independently confirmed builtin repairs

- Tencent's [Kimi Chat comparison](https://cloud.tencent.com/document/product/1823/132232)
  gives K3/K3.1's exact output default **131072** and ceiling **1048576**,
  and K2.8/K2.7's default **32768**. Preserve the K2.7 ceiling **262144**.
  The [reasoning guide](https://cloud.tencent.com/document/product/1823/131208),
  [GLM guide](https://cloud.tencent.com/document/product/1823/132061), and
  [DeepSeek guide](https://cloud.tencent.com/document/product/1823/132248)
  also establish exact Chat reasoning/vision flags and effort menus.
  Keep Chat capacities, defaults, and menus off Messages/Responses.
  Because route policies can only narrow intrinsic capabilities, newly
  confirmed model flags require explicit narrowing on other protocols to
  preserve their prior effective behavior. No protocol eligibility changes.
  GLM-5.2 accepts aliases rather than seven distinct reasoning strengths;
  HY3 maps low to high with tools. Contradictory K3/GLM default-effort
  descriptions are not resolved by inventing a default.
- Qianfan's [model table](https://cloud.baidu.com/doc/qianfan/s/rmh4stp0j)
  and [v2 Chat schema](https://cloud.baidu.com/doc/qianfan-api/s/3m7of64lb)
  establish 15 missing same-endpoint IDs: five DeepSeek, two ERNIE thinking,
  four GLM, and four Qwen 3.5. These are confirmed coverage omissions, not
  a claim that all were released today. Only exact published output ceilings
  are accepted; abbreviated contexts and undocumented defaults stay unknown.
  Four old ERNIE context integers contradict the current table and are
  cleared rather than replaced with a guessed unit conversion; ERNIE 5.0
  also gains its documented reasoning capability. Preserve X1.1 and V3.2
  routes: an upcoming retirement or absence from a current table is not
  an already-effective retirement.
- The [Together M3 card](https://www.together.ai/models/minimax-m3)
  confirms request-time thinking on exact serverless `MiniMaxAI/MiniMax-M3`.
  Enable reasoning only; do not invent effort controls or new token limits.
- The [Fireworks M3 card](https://fireworks.ai/models/fireworks/minimax-m3)
  and [official index](https://fireworks.ai/models) jointly support the
  **512000** advisory output ceiling. The absent local default, runtime
  output **8192**, existing image flag, and other Fireworks models remain
  unchanged. See [capacity evidence](metadata-evidence.md) for the distinction
  between output capacity, a service default, and Holon's budget.

### Registered endpoint rechecks and unresolved boundaries

- Independently rechecked DashScope Beijing
  [personal](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview)
  and [team](https://help.aliyun.com/zh/model-studio/token-plan-team-overview)
  eligibility; current confirmed text IDs are covered. The
  [team quickstart](https://help.aliyun.com/zh/model-studio/token-plan-team-quickstart)
  now documents shared Beijing OpenAI/Anthropic hosts, so the older
  console-only wording below must not be read as a current universal claim.
  Actual subscriptions still follow their console/deployment; Singapore
  remains separate and unregistered. The decision-model `systemone`
  request schema is not a natural-language Messages route. Preserve exact
  DeepSeek cache defaults, Qwen caching, and six historical unconfirmed routes.
- Rechecked Volcengine Coding and Agent Plan separately; their already
  accepted K3.1/other IDs remain covered. `doubao-seed-evolving`'s **1024k /
  256k** and economy's **1049k** do not establish new exact integers.
  Do not add Anthropic registrations from a documentation example.
- Rechecked the Tencent [protocol matrix](https://cloud.tencent.com/document/product/1823/130079)
  separately from its Chat field guides: no new eligible model ID.
  Code Plan is not an extra registered TokenHub endpoint. Partial Messages
  caching documentation does not guarantee complete explicit-cache semantics.
- Xiaomi's Responses enum and Token Plan still distinguish the three
  ordinary V2.6 IDs from the two subscription text IDs and region domains.
  Existing effort mappings/retirement filters remain; **1M** is not a new
  exact context certification. BigModel GLM FlashX Legal and the Z.AI
  counterpart remain candidates without exact Messages/plan eligibility;
  ordinary API and October 16 key categories are not interchangeable.
- MiniMax international/China and their M Plans were independently
  rechecked. Existing M3/M3.1 limits, effort and cache policies remain;
  native retirements are not copied onto hosted or plan models.
  Moonshot's six current IDs and prior retirement filters remain covered;
  October 31 is a new-account restriction, not today's global retirement.
- Rechecked OpenCode Go's independent Chat/Messages/Responses source table;
  K3.1/Iceman remain Chat-only. Discovery now lists Iceman but does not prove
  token capacity. Kilo's five public Auto limits still match. Zen, Kimi Code,
  and MiniMax's unregistered OpenAI interface do not create runtime endpoints.
- Together/Fireworks remaining hosted-capacity candidates stay unresolved;
  authenticated metadata failures were not bypassed. Dedicated Together
  M2.7 context and unusual Fireworks context integers are not substituted
  into unrelated serverless policies.

### Post-repair metadata review

- Compared all changed raw model/route fields, effective facts, and evidence
  against the previously reviewed baseline before updating it: 80 rows
  changed, including 15 new Qianfan routes; 520 existing rows were preserved
  without modification and no reviewed route was removed.
- The changed rows comprise 59 TokenHub, 19 Qianfan, and the two exact hosted
  routes above. Of these, 18 Messages and 17 Responses rows changed only
  their raw narrowing policies and provenance; their effective facts stayed
  identical. Chat defaults, effort, vision, and reasoning did not leak.
- The independent review additionally caught an intrinsic-only automatic
  preference check and the K3 output-field mismatch. Explicit-route default
  candidates now honor endpoint context/capability narrowing, retaining
  `tencent-tokenhub@responses/hy3`. Only the three exact Tencent K3 Chat
  routes send `max_completion_tokens`, including budget estimation and
  clipping; all other Chat routes retain `max_tokens`. Regressions cover
  compiled-in/generated preferences and local wire budgets without paid probes.
- Unknown capacities retain explicit owner/date/evidence/recheck records.
  The baseline migration is not an upstream capacity certification; the
  grouped semantic review above precedes the migration. Final verification
  passed: catalog 181 passed / 1 ignored, models.dev 67 passed, route-cache
  17 passed, OpenAI transport 52 passed, and Chat Completions 41 passed.
  The 600-route raw/effective/evidence projection matches the reviewed baseline;
  `coverage --check`, `cargo fmt --all -- --check`,
  `RUSTFLAGS="-D warnings" cargo check --locked --all-targets`, and
  `git diff --check` passed. Independent follow-up review confirmed both
  final P2 findings closed with no new blockers. The 24-request local HTTP
  mock regression is not a provider-hosted inference acceptance test.

## 2026-10-25 incremental review

This cycle starts from main `bc1a3dbe`, including #3729's exact Token Plan
DeepSeek cache defaults. Preserve those defaults and Qwen's existing explicit
cache behavior; a generic catalog refresh is not a replacement for that policy.

- Initial models.dev refresh revision
  `02fb1f3db076358d5dd0440889d3e9be0c8d4513bb3e47cd29bb3680027a607e`.
  The artifact retains 1,081 records: two Venice additions
  (`kimi-k3-1-pro` and `kimi-k3-1-pro-highspeed`), two upstream OpenRouter
  removals (`openai/gpt-3.5-turbo` and `openai/gpt-3.5-turbo-instruct`),
  and three changed records. Venice metadata remains `remote_discovered`
  upstream evidence, not a subscription qualification or independently
  verified endpoint specification. All 54 supplement models, 16 explicit
  supplement routes, artifact routes, aliases, and preferred selections are
  semantically unchanged by that initial refresh, before the independently
  reviewed additions below.
- Final refresh revision after the reviewed source changes:
  `4d78111a562730683698be7a3815b6741b29aa026b988335ceceb32840ac0462`.
  This is the snapshot SHA-256 and the artifact/supplement upstream revision.
  The final artifact contains 1,081 records, with two additions, two removals
  and four metadata changes from the baseline, including the K3 default-output
  correction below. All 54 supplement models and 16 explicit routes retain
  their previous semantics.
- Compared the three changed OpenRouter records with this cycle's 466-entry
  [public model list](https://openrouter.ai/api/v1/models).
  MiniMax M2.5 retains context 204,800 and changes output 32,768 → 131,072;
  `~z-ai/glm-latest` and `~z-ai/glm-flash-latest` retain context 1,048,576
  and change output 943,718 → 131,072. Each matches the public
  `top_provider.max_completion_tokens`; both removed GPT-3.5 IDs are absent.
  This checks only changed records and removal presence, not the full catalog.
- Re-fetched [Codex model metadata](https://github.com/openai/codex/blob/main/codex-rs/models-manager/models.json):
  SHA-256 remains
  `943ca7fe1d19ed019054158303f0dbbd80b3bef43b3aa425a71aa7cb3fb52c2b`,
  byte-identical to October 24. The seven listed, non-retired models remain
  covered. Keep the published 272,000 default context separate from the
  optional/account-specific 872,000 maximum. The [lifecycle page](https://learn.chatgpt.com/docs/models)
  still distinguishes ChatGPT sign-in retirements from API access.
- Rechecked DeepSeek [pricing](https://api-docs.deepseek.com/quick_start/pricing/)
  and [Responses restrictions](https://api-docs.deepseek.com/guides/responses_api/).
  Legacy Flash aliases, 1M/384K notation, image support, Pro-only server search,
  and stateless Responses restrictions remain unchanged. No subscription
  entitlement or cache override is inferred from these native API contracts.
- Retrieved Mistral's [GLM-5.3-Flash model page](https://docs.mistral.ai/models/zai-glm-5-3-flash)
  successfully through the documentation reader. It confirms the exact
  `zai-glm-5-3-flash` ID, multimodal/function-calling support, and published
  1M context / 128k maximum-output labels.
  The separate Markdown fetch still failed with a TLS reset. The page does
  not establish the exact integer conversion or `low/high/max` effort menu:
  the retained 1,048,576 / 131,072 integers and effort values remain attributed
  to models.dev rather than independently confirmed exact Mistral values.

### Independently confirmed endpoint changes

- [TokenHub's protocol matrix](https://cloud.tencent.com/document/product/1823/130079)
  and [Kimi call reference](https://cloud.tencent.com/document/product/1823/132232)
  agree on `kimi-k3.1-pro` and `kimi-k3.1-pro-highspeed` for all three registered
  protocols. Do not add the inconsistent shortened `kimi-k3-pro` IDs in the
  general model table. Only Chat has independently checked output/effort
  parameters; a shared URL is not evidence of identical protocol parameters.
- Volcengine's independent [Coding](https://ark.volcengine.com/region:cn-beijing/docs/coding-plan-personal-plan-overview.md)
  and [Agent](https://ark.volcengine.com/region:cn-beijing/docs/agent-plan-personal-plan-overview.md)
  tables qualify `kimi-k3.1-pro` on the existing `coding` and `plan` Responses
  endpoints. Agent qualifies every tier, unlike the older K3 restriction.
  Their 1024k/128k notation does not establish exact integers or effort options;
  do not inherit native Moonshot limits or the provider-wide effort fallback.
- [DashScope's Messages allowlist](https://help.aliyun.com/zh/model-studio/anthropic-api-messages)
  confirms eleven missing ordinary-API IDs: `qwen3.8-2.4t-a95b`, `qwen3.8-27b`,
  `qwen3.8-max-0902`, `qwen-plus-latest`, `qwen-flash-2025-07-28`,
  `qwen3.5-flash-2026-02-23`, `qwen3.5-plus-2026-02-15`,
  `qwen3.5-plus-2026-04-20`, `qwen3.6-flash-2026-04-16`,
  `qwen3.6-plus-2026-04-02`, and `qwen3.7-flash-2026-07-15`.
  These are confirmed coverage gaps, not a claim of eleven new releases.
  Add only ordinary Messages routes; no subscription route follows from this
  list. The independent [2.4T](https://help.aliyun.com/zh/model-studio/qwen3-8-2-4t-a95b),
  [27B](https://help.aliyun.com/zh/model-studio/qwen3-8-27b), and
  [Max snapshot](https://help.aliyun.com/zh/model-studio/qwen3-8-max) cards
  additionally confirm exact 1,000,000 context / 131,072 maximum output and
  thinking support for the three Qwen3.8 IDs. Only 27B and Max have image
  input. Leave their effort menus and the other eight IDs' metadata unknown.
- The [international Moonshot lifecycle page](https://platform.kimi.ai/docs/models.md)
  directly confirms the August 31 retirement of `kimi-k2.5` and the seven
  `moonshot-v1` IDs (including auto and vision-preview). Remove only native
  static entries and reject their admission/retention in the supplement.
  K2.6/K2.7 remain available: October 31 new-account closure and a planned
  end-of-December sunset are not an October 25 retirement.
  The [international Chat schema](https://platform.kimi.ai/docs/api/chat.md)
  includes `kimi-k3` alongside both K3.1 IDs in the 131,072 default /
  1,048,576 maximum output contract. Extend the existing exact projection
  rule to K3 and correct its static fallback; its exact context integer is
  retained from the existing models.dev record, not inferred from “1M”.
- The [pinned OpenCode Go protocol table](https://github.com/anomalyco/opencode/blob/0093fe5782766f180167efab86fb8fdb3c5b6a4a/services/www/src/docs/content/console/go.mdx)
  adds `iceman-contributor-free` on Chat only. Record the limited-time free,
  training-eligible/non-ZDR boundary; exact limits, modalities, and effort
  remain unknown. Its absence from the successful public model-list response
  does not override the explicit official protocol table.
- Together's own [M3.1 Flash](https://www.together.ai/models/minimax-m3-1-flash)
  and [DeepSeek 1017](https://www.together.ai/models/deepseek-v4-1-flash-1017)
  pages provide exact serverless Chat IDs, image input, and reasoning support.
  M3.1 Flash documents low/medium/high/xhigh/max; DeepSeek documents a numeric
  1–100 range, which the current string-menu contract cannot faithfully
  represent, so its menu stays empty. Exact context integers and output
  ceilings remain unknown; the recommended DeepSeek 256K budget is not a cap.
  Together's full docs/model-list fetches remained blocked; this is not a
  zero-difference claim for its whole catalog.
- The [Fireworks model index](https://fireworks.ai/models) publishes exact
  `contextLength=512000` for `accounts/fireworks/models/minimax-m3`, matching
  the artifact and correcting the old static 524,288 fallback. Its image
  flag conflicts with the multimodal detail page, so retain the existing
  image capability rather than treating the index flag as a new restriction.

### Registered endpoint coverage and preserved boundaries

All rows are documentation checks, not authenticated account-availability tests.
Unregistered protocols or regions are not silently introduced.

| Endpoint | This cycle's result |
| --- | --- |
| `dashscope@default` | Eleven Messages coverage additions above; the existing host is still supported, not retired by the recommended workspace-domain migration. |
| `dashscope@token-plan` | Personal/team pages independently rechecked; no new qualified ID, no promotion of the six previously unconfirmed routes. Team deployments still use console-specific URLs. Preserve #3729 and Qwen policies. |
| `dashscope@coding-plan` | Domestic six-ID current list covered, including `qwen3.5-plus`; omission is not affirmative retirement of historical routes. |
| `dashscope@coding-plan-intl` | International list independently checked; distinct host/key retained, no inferred exact token or effort limits. |
| `bigmodel@default` | GLM-5.3 text / Flash multimodal boundaries unchanged; FlashX ordinary-API listing does not prove Messages or plan eligibility. No distinct coding endpoint is registered. |
| `zai@default` | International model/plan pages independently checked; FlashX still excluded from the plan. Distinct key classification remains significant despite the common Anthropic URL. |
| `xiaomi@default` | Three V2.6 IDs and exact 131,072 output contract covered; effort values other than none are compatibility mappings, not distinct reasoning strengths. Existing October 21 retirement filters remain. |
| `xiaomi@token-plan` | Pro/Flash qualify, Ultraspeed does not gain new qualification; only China is registered, not the documented Singapore/Amsterdam hosts. |
| `volcengine@default` | Ordinary Responses unchanged; subscription qualification and retirement notices do not apply to it. |
| `volcengine@coding` | K3.1 addition above; K2.7 Code remains callable until October 28 14:00 UTC+8, lowercase M3 until November 2 14:00 UTC+8. |
| `volcengine@plan` | Independent Agent K3.1 qualification above; preserve exact dated/uppercase IDs and GLM-5.2 forwarding. |
| `tencent-tokenhub@default` | Two K3.1 additions, 42 current matrix Chat IDs; three HY translation IDs remain Chat-only. |
| `tencent-tokenhub@messages` | Two additions, 39 current matrix IDs; Guangzhou host/key only, not an alias for Singapore or an unregistered Token Plan. |
| `tencent-tokenhub@responses` | Two additions, 32 current matrix IDs; GLM-5.2 remains compatibility mode. |
| `byteplus@coding` | All 13 official quick-start IDs covered; Seed Code vision omissions were already fixed. Keep published k notation unconverted; do not copy the Chat list into unregistered alternative protocols. |
| `stepfun@default` | Ordinary API remains separate from Plan; the router's Plan-only contract is unchanged. |
| `stepfun@plan` | Five official IDs covered. Plan Messages exists in documentation but is not registered. Router image/document/web-search restrictions and ignored output effort are not inherited from underlying models. |
| `minimax@default` | International Anthropic pages rechecked: M3/M3.1 covered, M2/M2.1 retirement preserved, M2.5/M2.7 existing-API access retained until November 18 despite conflicting FAQ footer. |
| `minimax-cn@default` | China pages independently checked; native M3/M3.1 defaults and exact cache policies retained. Do not infer international access from China evidence. |
| `minimax@m-plan` | Formal/legacy M3.1 qualified; native M3 ordinary-API availability does not qualify it for the plan. |
| `minimax-cn@m-plan` | China plan independently rechecked; region/key separation and formal/legacy mapping preserved. |
| `moonshot@default` | International retirement and K3 budget corrections above; domestic API and Kimi Code are not registered endpoints. |
| `opencode-go@default` | Iceman addition; K3.1 remains Chat-only. Google protocol is not registered. |
| `opencode-go@messages` | Current protocol allocations covered; native MiniMax retirements do not retire Go history. |
| `opencode-go@responses` | Grok/GPT/Muse allocations covered; no K3.1 or Iceman protocol expansion. |
| `kilocode@default` | Five Auto context/output pairs match the public gateway list; Free text-only/training and Small paid boundaries retained. Supported parameters are not an effort enum. |
| `together@default` | Two directly confirmed Chat additions above; unrelated video-generation MiniMax H3 excluded. |
| `fireworks@default` | M3 fallback context correction above; no family-wide retirement inferred from directory absence. |
| `openai-codex@default` | Seven current non-retired metadata entries covered; optional account-specific context is not the default. |
| `deepseek@default`, `deepseek@responses` | Native contracts rechecked above; no package qualification or cache policy inferred. |
| `openrouter@default`, `mistral@default` | Changed-record validation and Mistral exact-limit evidence gap retained above. |
| OpenCode Zen / Kimi Code | Catalog sources exist but no independent runtime endpoints are registered; no endpoint is added by this review. |

BytePlus sources: [quick start](https://docs.byteplus.com/en/docs/modelark/coding-plan-personal-get-started)
and [overview](https://docs.byteplus.com/en/docs/modelark/coding-plan-personal-plan-overview).
StepFun sources: [Plan integration](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api.md)
and [Messages restrictions](https://platform.stepfun.com/docs/zh/api-reference/chat/messages-create.md).
Other unchanged endpoint source links and unresolved historical evidence are
retained in the October 24 review below.

Validation for this cycle: built-in snapshot regeneration, refresh, and
`models-dev validate` pass (1,081 artifact models, 54 supplement models, 16
explicit supplement routes). `cargo test --locked --lib model_catalog::`
passes 165 tests with one intentional snapshot-writer ignore;
`cargo test --locked models_dev` passes 67 matching library tests, and
`cargo test --locked --lib route_cache` passes 17 tests.
After formatting the corrected test fixture, `cargo fmt --all -- --check`,
`RUSTFLAGS="-D warnings" cargo check --locked --all-targets`, and
`git diff --check` all pass. Independent final review found no blocking
defect; its sole low-priority audit revision mismatch has been corrected.

## 2026-10-24 incremental review

- Refreshed models.dev revision
  `3800890225bd879647209b9559344168f6e60f41a6b46a25c2e016707265f92b`.
  The artifact has 1,081 models: 10 additions, 20 upstream removals, and five
  changed records. These generic catalog removals do not retire independently
  reviewed subscription routes. The supplement has 54 models (three new
  drafts, 51 retained) and the same 16 explicit routes.
- Compared all six added or changed OpenRouter records with this cycle's
  [public model list](https://openrouter.ai/api/v1/models). Context and
  `top_provider.max_completion_tokens` match for Kimi K3.1 Pro, MiniMax M3.1
  Flash, Qwen 3.5 9B, Qwen 3.8 27B, GLM-5.3, and GLM-5.3-Flash. This is
  a changed-record comparison, not a full-catalog zero-diff claim.
- New generic draft `mistral/zai-glm-5-3-flash` has upstream context
  1,048,576, output 131,072, and explicit effort options `low/high/max`.
  Mistral's [model page](https://docs.mistral.ai/models/zai-glm-5-3-flash)
  is discoverable with the exact ID and identifies an unmodified multimodal
  model. Full-page retrieval failed with a TLS connection reset; exact limits
  and effort values remain attributed to models.dev, not independently
  verified Mistral contract values.
- Two other generic drafts are `moonshot/kimi-k3.1-pro` and
  `moonshot/kimi-k3.1-pro-highspeed`. The international [model list](https://platform.kimi.ai/docs/models),
  [Chat API](https://platform.kimi.ai/docs/api/chat), and
  [parameter table](https://platform.kimi.ai/docs/api/models-overview) independently
  confirm both IDs at `api.moonshot.ai/v1`, 1,048,576 context, always-on thinking,
  and top-level `reasoning_effort=low/high/max` (default `max`).
  The API explicitly distinguishes the 131,072 default generation budget
  from the configurable 1,048,576 ceiling. Preserve that distinction in
  projection and retained supplements rather than using the upstream ceiling
  as the runtime request default. No new subscription route is inferred.
  The legacy `platform.moonshot.ai` documentation returned HTTP 403, but the
  linked `platform.kimi.ai` pages and full Chat API Markdown were retrieved
  successfully and explicitly use the international API base URL; these
  findings do not rely on domestic Kimi API or Code Plan documentation.
- Codex metadata is byte-identical to the previous cycle (SHA-256
  `943ca7fe1d19ed019054158303f0dbbd80b3bef43b3aa425a71aa7cb3fb52c2b`);
  its path history still identifies revision `d63a9b83`. The existing
  seven listed, non-retired models remain covered. Keep published default
  context 272,000 distinct from maximum 872,000 and account-specific access.
  Rechecked [lifecycle](https://learn.chatgpt.com/docs/models) information
  still distinguishes ChatGPT-sign-in retirements from API-key access.
- Rechecked DeepSeek's [pricing](https://api-docs.deepseek.com/quick_start/pricing/)
  and [Responses contract](https://api-docs.deepseek.com/guides/responses_api/):
  legacy Flash aliases, 1M/384K notation, image input, Pro-only server search,
  and stateless Responses restrictions remain unchanged.

### Endpoint-specific findings

The two endpoint review groups re-fetched public evidence on October 23,
20:34–20:41 UTC. No credentials or inference calls were used.

- MiniMax's independently checked [international](https://platform.minimax.io/docs/guides/text-generation.md)
  and [Chinese](https://platform.minimax.cn/docs/guides/text-generation.md)
  model tables both list `MiniMax-M3` and `MiniMax-M3.1-Flash`. Add the
  previously missing Chinese **default API** M3 route with context 1,000,000,
  recommended output 131,072 and maximum output 524,288. Do not add M3 to
  M Plan or copy M3.1's five effort options into M3.
- Both regional Anthropic schemas
  ([international](https://platform.minimax.io/docs/api-reference/text-chat-anthropic.md),
  [China](https://platform.minimax.cn/docs/api-reference/text-chat-anthropic.md))
  explicitly exclude M3/M3.1 from `cache_control` support. Set the built-in
  **exact route** policy to false for each region's default M3/M3.1 and M Plan
  M3.1. The legacy Preview subscription ID forwards to formal M3.1 according
  to each region's [M Plan introduction](https://platform.minimax.io/docs/m-plan/intro.md)
  ([China](https://platform.minimax.cn/docs/m-plan/intro.md)); apply the same
  conservative default there, without claiming a separately exercised cache
  contract. User route/provider overrides remain higher priority; implicit
  caching and M2-family routes are unaffected.
- The international [retirement FAQ](https://platform.minimax.io/docs/faq/models.md)
  explicitly lists `MiniMax-M2`, `MiniMax-M2.1`, and
  `MiniMax-M2.1-highspeed` as retired, without an effective date. Remove their
  native international catalog entries and prevent supplement reintroduction,
  not their aggregator counterparts or manually configured references.
  Keep M2.5/M2.7: the same page's dated existing-API clause allows them until
  **November 18, 2026**, despite its conflicting bottom retirement table.
  Subscription migration on October 19 does not retire ordinary API access.
- OpenCode Go's [pinned official endpoint table](https://raw.githubusercontent.com/anomalyco/opencode/755bbc743a497526262461852b52b0c7995e27d1/services/www/src/docs/content/console/go.mdx)
  adds `kimi-k3.1-pro` for Go and Go Plus, **Chat only**. Register only that
  protocol; token limits, modalities, and reasoning controls remain unknown,
  not inherited from native Kimi or Kimi Code. Keep existing historical Go
  IDs. Public Go discovery still returns 403, not retirement evidence.
- Volcengine's separate [Coding retirement notice](https://ark.volcengine.com/region:cn-beijing/docs/coding-plan-personal-model-deprecation.md)
  and [Agent retirement notice](https://ark.volcengine.com/region:cn-beijing/docs/agent-plan-personal-model-deprecation-announcement.md)
  explicitly retired exact `kimi-k2.6` on **August 18, 2026, 14:00 UTC+8**.
  Remove both subscription routes, including retained supplement routes,
  without claiming ordinary API or other providers' retirement. Retain
  `glm-5.2` as a documented compatibility ID forwarding to GLM-5.3, not as
  original GLM-5.2 service.
- Those independent notices also document `minimax-latest`, `kimi-latest`,
  and `deepseek-latest`; add these subscription router aliases with unknown
  token limits and no inherited model capabilities. Keep future retirements:
  `kimi-k2.7-code` ends **October 28, 2026, 14:00 UTC+8** and lowercase
  `minimax-m3` ends **November 2, 2026, 14:00 UTC+8**. Earlier new-user
  cutoffs do not terminate existing users; do not infer retirement of
  dated IDs or uppercase `MiniMax-M3` aliases.

### Other registered endpoints and evidence boundaries

| Endpoint(s) | Independently rechecked evidence and preserved boundary |
|---|---|
| `bigmodel@default`, `zai@default` | Separate [China](https://docs.bigmodel.cn/cn/coding-plan/latest-model.md) / [international](https://docs.z.ai/devpack/latest-model.md) guides still confirm GLM-5.3/Flash and `low/high/max`. The 1,000,000 client context configuration is not proof of every historical model's service limit. Exact subscription output limits remain unconfirmed; no unregistered Chat/Responses or coding-plan endpoint is added. |
| DashScope `@coding-plan`, `@coding-plan-intl` | Separate [China](https://help.aliyun.com/zh/model-studio/coding-plan) / [international](https://www.alibabacloud.com/help/en/model-studio/coding-plan) lists remain covered. Regional hosts and subscription credentials stay separate; current-list absence does not retire historical IDs. |
| DashScope `@token-plan` | [Personal](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview) / [team](https://help.aliyun.com/zh/model-studio/token-plan-team-overview) lists remain covered, including dated DeepSeek. Keep `qwen3.8-max-preview` because requests forward to max. Team deployment URLs remain console-specific; the fixed local host is not claimed to cover every team deployment. Six previously unconfirmed supplement routes gain no new eligibility. Preserve exact GLM-5.3 cache policy and user precedence from #3705/#3706. |
| MiniMax regional `@m-plan` | Separate [international](https://platform.minimax.io/docs/m-plan/quickstart.md) / [China](https://platform.minimax.cn/docs/m-plan/quickstart.md) credentials and two formal/legacy M3.1 IDs remain covered. Regional [existing Token Plan](https://platform.minimax.io/docs/m-plan/token-plan-notice.md) ([China](https://platform.minimax.cn/docs/m-plan/token-plan-notice.md)) access is not an unregistered token-plan endpoint. Preview output limits remain unknown. |
| `xiaomi@token-plan` | [Subscription table](https://mimo.mi.com/docs/en-US/tokenplan/Token%20Plan/subscription) still confirms V2.6 Pro/Flash, not Ultraspeed eligibility. Official client catalog keeps 1,048,576 context and `none/low/medium/high`; [Responses](https://mimo.mi.com/docs/en-US/api/chat/responses) documents 131,072 output. No new regional endpoint or historical Ultraspeed retirement is inferred. |
| `stepfun@plan` | [Reasoning integration](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api.md) still lists the five covered models. Step5/3.7 have three efforts; 2603 only low/high. “1M”/“250K” do not independently establish exact integer service limits; no Messages endpoint is added. |
| `moonshot@default` | International [models](https://platform.kimi.ai/docs/models), [Chat API](https://platform.kimi.ai/docs/api/chat), and [parameters](https://platform.kimi.ai/docs/api/models-overview) confirm the two new generic K3.1 IDs, 1M context, `low/high/max`, default output 131,072 and configurable ceiling 1,048,576. The former `platform.moonshot.ai` retrieval failure no longer blocks these new official sources. |
| Kimi Code | The separate [subscription model table](https://www.kimi.com/code/docs/en/kimi-code/models.html) names `k3.1-pro` / `k3.1-pro-highspeed` without the generic `kimi-` prefix, with `low/high/max` (default `max`) and tier-dependent 1M access. China/international hosts are `api.kimi.com/coding/v1` / `api.kimi.ai/coding/v1`; neither endpoint is registered. Do not copy generic output defaults/ceilings, model IDs, qualification, or credentials into them. |
| `byteplus@coding` | Independent international [overview](https://docs.byteplus.com/en/docs/modelark/coding-plan-personal-plan-overview) covers 13 IDs. GPT-OSS-120B remains Chat-only; do not apply mainland Volcengine retirement or interpret OpenCode configuration limits as service hard limits. |
| Tencent PAYG `@default`, `@messages`, `@responses` | [Protocol table](https://cloud.tencent.com/document/product/1823/130079) covers 40 Chat / 37 Messages / 30 Responses IDs. All current rows are covered; Responses equals the local allowlist. Retain ten additional historical Chat IDs without retirement evidence. Guangzhou/Singapore hosts and credentials remain independent; unregistered personal/enterprise Token Plans do not qualify PAYG models or authorize new endpoints. |
| OpenCode Go `@messages`, `@responses` | The pinned table's other registered protocol assignments match. Historical free IDs remain distinct. Gemini's Google protocol is unregistered; K3.1 is not added to these two endpoints. |
| `kilocode@default` | [Public API](https://api.kilo.ai/api/gateway/models) confirms all five Auto routers and their limits. Small is paid; Free alone has no image input and may train on prompts. No fixed effort menu is inferred from reasoning-parameter support. |
| `openrouter@default` Auto | [Public API](https://openrouter.ai/api/v1/models) still publishes `openrouter/auto`, context 2,000,000 and null maximum completion tokens. Short `auto` request compatibility remains unverified; do not migrate persistent references or label artifact fallback as a hard limit. |

Final built-in snapshot regeneration and `models-dev refresh`/`validate` passed
with 1,081 artifact models and 54 supplement models. The final locked
`model_catalog` run passed 158 tests with one intentional snapshot-writer ignore;
`route_cache` passed all 16 tests. Format, diff, and warning-free locked
all-targets checks passed. Incremental independent review found no blocking
defects; the GLM-5.2 forwarding descriptions are covered for both subscription
endpoints in source, regression assertions, and the regenerated snapshot.

## 2026-10-23 incremental review

- Refreshed models.dev revision
  `fb43d87ae4f5d28f2467baedce982615c7ed30cd492e05ca27c0cc4cbbc6fa0b`.
  The artifact retains 1,091 models: four additions, four removals, and one
  changed record relative to the merged catalog. The additions are three
  Fireworks spot routers and the OpenRouter formal MiniMax ID; removals are
  three Gemini Veo previews and the OpenRouter MiniMax `:free` ID. OpenRouter
  GLM-5.3's output limit changes from 943718 to 262144. Only the two added or
  changed OpenRouter records were compared with the public API's context and
  `top_provider.max_completion_tokens`; this is not a full-catalog zero-diff
  claim. These upstream changes are not subscription eligibility evidence.
  The supplement retains 51 models and 16 explicit routes, with no semantic
  model or route changes.
- The official Codex `models-manager/models.json` is byte-identical to the
  previous cycle (SHA-256
  `943ca7fe1d19ed019054158303f0dbbd80b3bef43b3aa425a71aa7cb3fb52c2b`).
  The [lifecycle page](https://learn.chatgpt.com/docs/models) continues to
  distinguish GPT-5.5's October 14 ChatGPT-sign-in retirement from API-key
  access. No hidden model, account-specific window, or new Codex route is added.
- [DeepSeek pricing](https://api-docs.deepseek.com/quick_start/pricing/) and
  [Responses](https://api-docs.deepseek.com/guides/responses_api/) retain the
  reviewed Flash legacy aliases, 1M/384K notation, Flash image support, Pro-only
  server web search, and stateless Responses restrictions. No route change.
- Xiaomi's [Responses parameters](https://mimo.mi.com/docs/en-US/api/chat/responses)
  and the [official client model JSON](https://example-files.cnbj1.mi-fds.com/example-files/configs/model-catalogs.json),
  linked by its [Codex setup](https://mimo.mi.com/docs/en-US/tokenplan/integration/codex-configuration),
  confirm the `none/low/medium/high` menu for V2.6 Pro, Flash, and Pro Ultraspeed.
  Add these options to the exact native route policy, including existing
  Pro/Flash Token Plan routes but not Ultraspeed Token Plan or aggregators.
  The API accepts additional aliases: `minimal` maps to `low`, and
  `xhigh/max/ultra` map to `high`. `none` disables thinking; the remaining
  values currently do not distinguish actual thinking intensity. The client
  menu is not proof of distinct server budgets, and its `low` default is not
  promoted to a service default. The documented output default/maximum is
  131072; the 1048576 context integer remains client-configuration evidence,
  not an independently verified service hard limit.

China endpoint evidence was checked on October 22, 20:34–20:42 UTC
(October 23 Beijing time):

| Endpoint | Result and preserved boundary |
| --- | --- |
| DashScope `coding-plan` | [Mainland table](https://help.aliyun.com/zh/model-studio/coding-plan) still lists the same six IDs. Keep reviewed history; six previously unconfirmed supplement routes remain unconfirmed. |
| DashScope `coding-plan-intl` | [International table](https://www.alibabacloud.com/help/en/model-studio/coding-plan) independently confirms the same six IDs, with vision on the first three Qwen IDs and Kimi. Keep its `coding-intl` domains and plan-specific key separate; no exact token/effort limit evidence. |
| DashScope `token-plan` | The [personal table](https://help.aliyun.com/zh/model-studio/token-plan-personal-overview) now directly lists `deepseek-v4-pro-0813`, superseding the previous team-only evidence note. Retain the union with the [team table](https://help.aliyun.com/zh/model-studio/token-plan-team-overview), `auto`, and reviewed history. No route changes. |
| Volcengine `coding` / `coding-plan` | [Current overview](https://docs.volcengine.com/docs/ark/coding-plan-personal-plan-overview?lang=zh) includes the already-added Economy route. Both `MiniMax-M3` and `Kimi-K2.7-Code` are marked for future removal without an effective date; do not remove them. |
| Volcengine `code-plan` spelling | [Alternate page](https://docs.volcengine.com/docs/ark/code-plan-personal-plan-overview?lang=zh) still yielded only a JavaScript shell. Its equivalence is not independently established; no new endpoint. |
| Volcengine `plan` | [Agent Plan](https://docs.volcengine.com/docs/ark/agent-plan-personal-plan-overview?lang=zh) confirms multimodal Economy and only `1049k/384k` notation, not exact integers or effort enums. Preserve seven historical dated routes. Enabling excess postpaid use permits charges on the same URL/key; this review did not enable it. |
| Tencent TokenHub default / `messages` / `responses` | [Protocol matrix](https://cloud.tencent.com/document/product/1823/130079) lists 40/37/30 IDs respectively. The 30 Responses IDs exactly match the local allowlist; all current IDs are covered. Retain history absent from the current table, exclude the three Chat-only `hy-mt2-*` IDs from Messages, and do not inherit original-vendor capabilities or effort. |
| Xiaomi native | Add the route-scoped effort menu above. Both V2.5 native chat IDs were already removed after the [October 21, 10:00 Beijing retirement](https://mimo.mi.com/docs/en-US/updates/deprecate); do not repeat the removal or affect Tencent's same-named routes. |
| Xiaomi `token-plan` | [Subscription table](https://mimo.mi.com/docs/en-US/tokenplan/Token%20Plan/subscription) preserves Pro/Flash eligibility. [OpenCode setup](https://mimo.mi.com/docs/en-US/tokenplan/integration/opencode) additionally lists the European `token-plan-ams.xiaomimimo.com` domain; this alone does not establish regional model parity or authorize a new endpoint. No Ultraspeed plan route. |
| Z.AI `coding-plan` | [Model table](https://docs.z.ai/devpack/latest-model.md) retains text GLM-5.3, image GLM-5.3-Flash, configured context 1000000, and `low/high/max`. Chat/Responses alternative URLs remain an unregistered coverage gap, not an implicit expansion. [Key types](https://docs.z.ai/devpack/notice/api-key-management.md) remain separate from URL selection. |
| BigModel `coding-plan` | Independently checked the [regional model table](https://docs.bigmodel.cn/cn/coding-plan/latest-model.md) and [key rules](https://docs.bigmodel.cn/cn/coding-plan/notice/api-key-management.md), rather than copying Z.AI eligibility. New standard/personal/team keys since October 16 are billed by key type; changing URLs does not change that purpose, and legacy keys have separate compatibility rules. No route changes. |

Other endpoint evidence was checked on October 22, 20:34–20:50 UTC
(October 23 Beijing time):

| Endpoint | Result and preserved boundary |
| --- | --- |
| MiniMax international `default` / `m-plan` | The [international schema](https://platform.minimax.io/docs/api-reference/text-chat-anthropic.md) explicitly gives both M3 and M3.1 a recommended output of 131072 and maximum 524288. Correct the existing native M3's 32768 values; do not copy M3.1's effort menu to M3 or change aggregator limits. Formal M3.1 was already correct. The [subscription guide](https://platform.minimax.io/docs/m-plan/quickstart.md) requires a separate Subscription Key; [Preview forwarding](https://platform.minimax.io/docs/m-plan/intro.md) remains subscription compatibility, not a new general API route. |
| MiniMax China `default` / `m-plan` | Independently checked the [regional schema](https://platform.minimaxi.com/docs/api-reference/text-chat-anthropic.md) and [subscription guide](https://platform.minimaxi.com/docs/m-plan/quickstart.md): formal M3.1 and separate subscription credentials remain correct. [Existing Token Plans](https://platform.minimaxi.com/docs/m-plan/token-plan-notice.md) do not require inventing another endpoint. No new regional M3 route. |
| Moonshot international `default` | The [international model page](https://platform.moonshot.ai/docs/models.md) still returns 403. China's [model table](https://platform.kimi.com/docs/models.md) and [K3 guide](https://platform.kimi.com/docs/guide/kimi-k3-quickstart.md) do not establish international retirement, eligibility, or limits. Preserve reviewed routes and record this regional evidence gap. |
| StepFun `plan` | The [Plan integration](https://platform.stepfun.com/docs/zh/step-plan/integrations/reasoning-api.md) confirms the existing Chat address and five IDs. [Messages](https://platform.stepfun.com/docs/zh/api-reference/chat/messages-create.md) is separately documented but unregistered; do not silently expand protocol coverage or turn “250K” into an integer hard limit. |
| OpenCode Go `default` / `messages` / `responses` | The [official protocol table](https://github.com/anomalyco/opencode/blob/074f2fe524892a7efc03b2709b86008188ee9057/services/www/src/docs/content/console/go.mdx), updated after the previous cycle, confirms formal `minimax-m3.1-flash` on Chat only. Add that exact ID; retain the distinct historical Free ID. Advisory context 1000000, output 512000, image input, and reasoning come from the Go-specific models.dev entry, not MiniMax's native API. No new Messages/Responses route or effort enum. Other registered protocol assignments remain supported; direct discovery still returns 403, so this is not a live model-count claim. |
| Kilo `default` | The [public model API](https://api.kilo.ai/api/gateway/models) still confirms all five Auto IDs and their reviewed limits. Free is text-only and may use data for training; Small is paid. No fixed effort enum or new route. |
| OpenRouter `default` | The [public API](https://openrouter.ai/api/v1/models) lists native `openrouter/auto`, context 2000000, and a null completion ceiling. The existing short model ID `auto` remains a compatibility uncertainty, not proven request failure. Record it without silently migrating stored model references or treating artifact fallback output values as an official ceiling. |

Unregistered coverage and remaining evidence boundaries:

- Kimi Code's [Chinese](https://www.kimi.com/code/docs/) and
  [English](https://www.kimi.com/code/docs/en/) overviews separately document
  `api.kimi.com/coding/v1` and `api.kimi.ai/coding/v1` for Chat/Responses,
  with `/coding/` bases for Messages. Its Code key is separate from Moonshot's
  prepaid API; cross-region key interchangeability is unconfirmed. The
  [model settings](https://www.kimi.com/code/docs/kimi-code/models.html) and
  [membership rules](https://www.kimi.com/code/docs/kimi-code/membership.html)
  confirm tier-specific K3 context and default effort `high`, not native K3's
  `max`. Neither regional endpoint is added in this incremental repair.
- OpenCode's [pay-as-you-go table](https://github.com/anomalyco/opencode/blob/074f2fe524892a7efc03b2709b86008188ee9057/services/www/src/docs/content/console/models.mdx)
  is independent of Go: same-named M3 and Qwen models can use different protocols.
  Zen/Console remains unregistered; no Go eligibility is copied across.
- Go documents a real client User-Agent and stable `x-opencode-session`.
  Runtime registration alone does not demonstrate these client requirements
  or authenticated subscription usability; no live inference was performed.

Validation for this cycle: built-in snapshot regeneration and `models-dev
validate` pass (1,091 artifact models, 51 supplement models, 16 explicit
supplement routes, no stale mappings). `cargo test --locked --lib
model_catalog::` passes 134 tests with one intentional snapshot-writer ignore;
`cargo test --locked models_dev` passes 63 matching library tests.
`cargo fmt --all -- --check`, `RUSTFLAGS="-D warnings" cargo check --locked
--all-targets`, and `git diff --check` pass. Independent read-only review and
the focused Go/M3 follow-up found no confirmed defects. Generated metadata and
routes match the reviewed source; supplement changes are provenance-only.

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
