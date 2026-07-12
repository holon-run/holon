# StepFun model catalog

Holon models the StepFun pay-as-you-go and Step Plan surfaces as routes of the
canonical `stepfun` provider. The legacy `stepfun-plan` provider remains an
alias for the `plan` endpoint so existing configuration continues to resolve
to canonical `stepfun/*` model references.

The chat catalog contains the three current models documented for both
surfaces: `step-3.7-flash`, `step-3.5-flash-2603`, and `step-3.5-flash`.
Step Plan also documents audio, image-editing, and router models, but Holon's
OpenAI Chat Completions provider does not project models whose primary
contracts use other endpoints.

`step-3.7-flash` supports text, image, and video input. Holon's current
modality model records its image-input capability and its documented
`low` / `medium` / `high` reasoning effort values. The
`step-3.5-flash-2603` variant records its documented `low` / `high` values.
The base `step-3.5-flash` remains a fixed reasoning model because StepFun does
not publish a named effort selector for it.

All three models have a documented 256K total context window. StepFun does not
publish a separate maximum output-token limit on these model pages, so Holon
does not retain the previous inferred 65,536-token output limit.

The built-in endpoints use StepFun's current `api.stepfun.com` host. The older
`api.stepfun.ai` host is not retained because it is absent from the current
official API and Step Plan documentation.

Sources (checked 2026-07-12):

- Model capability overview:
  `https://platform.stepfun.com/docs/zh/guides/models/overview`
- Step 3.7 Flash:
  `https://platform.stepfun.com/docs/zh/guides/models/step-3.7-flash`
- Step 3.5 Flash and 2603 variant:
  `https://platform.stepfun.com/docs/zh/guides/models/step-3.5-flash`
- Reasoning model overview:
  `https://platform.stepfun.com/docs/zh/guides/models/reasoning`
- Step Plan overview and model allowlist:
  `https://platform.stepfun.com/docs/zh/step-plan/overview`
