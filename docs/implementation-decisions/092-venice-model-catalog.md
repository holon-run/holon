# Venice model catalog

Holon models Venice as an OpenAI Chat Completions provider at
`https://api.venice.ai/api/v1`. The built-in catalog contains only the current
stable text models marked by Venice with the `default`, `default_reasoning`,
`default_vision`, `default_code`, or `most_uncensored` traits. This avoids
copying Venice's large, fast-changing recommendation and beta inventory into
static runtime data.

Runtime discovery uses `GET /models?type=text`. It imports only online text
models that have not reached their published removal time and maps context,
output limit, vision, reasoning, and reasoning-effort options only when Venice
reports those fields. Image, video, audio, embedding, upscale, and inpaint
model types remain excluded because they require task-specific transports
rather than Chat Completions.

Sources:

- <https://docs.venice.ai/api-reference/endpoint/chat/completions>
- <https://docs.venice.ai/api-reference/endpoint/models/list>
- <https://docs.venice.ai/models/text>
- <https://docs.venice.ai/overview/deprecations>
