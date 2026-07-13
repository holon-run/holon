# Tencent TokenHub model catalog

Holon models Tencent TokenHub as one canonical provider with two compatible
transport endpoints. The default endpoint uses OpenAI Chat Completions at
`https://tokenhub.tencentmaas.com/v1`, while the `messages` endpoint uses
Anthropic Messages at `https://tokenhub.tencentmaas.com`. Models remain
addressable as `tencent-tokenhub/<model>` and have routes through both
endpoints.

The static catalog follows the current official TokenHub model table and keeps
only language, reasoning, coding, role-play, translation, and image
understanding models usable through Holon's existing turn transports. Image,
video, and 3D generation models, embedding models, and retired language models
are excluded because their task contracts are either unsupported by those
transports or no longer current.

Published context and output limits are recorded when the official table states
them. Unknown limits remain unset. Reasoning support is intrinsic model
metadata, but TokenHub does not publish one portable reasoning-effort control
shared by both transports, so Holon does not invent effort levels.

Authenticated `GET /v1/models` discovery returns model identifiers without
portable capability or limit metadata. Discovery therefore acts as a
conservative intersection with the reviewed static table and does not infer
capabilities from names. Static metadata remains responsible for intrinsic
capabilities and limits.

Sources reviewed on 2026-07-13:

- <https://cloud.tencent.com/document/product/1823/130051>
- <https://cloud.tencent.com/document/product/1823/132252>
- <https://cloud.tencent.com/document/product/1823/130078>
- <https://tokenhub.tencentmaas.com/v1/models>
