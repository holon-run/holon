# Together AI model catalog

Holon snapshots a focused set of current Together AI serverless chat models
from the official serverless model table. The snapshot was verified on
2026-07-13.

The built-in picker includes current general chat, reasoning, and vision
representatives. Context windows follow Together's serverless table. Together
does not publish a distinct maximum generation-token limit there, so Holon
leaves the output limit unset rather than reusing the context length or an
example request value.

Intrinsic reasoning capability follows Together's reasoning guide, while image
input follows the serverless vision table. The current Holon Together route
uses the OpenAI Chat Completions transport, which does not send Together's
`reasoning`, `chat_template_kwargs`, or `reasoning_effort` controls. The route
therefore does not advertise adjustable effort levels, including for GPT-OSS,
even though Together documents that API capability.

Former GLM 4.7, Kimi K2.5 and K2 0905, DeepSeek V3.1 and R1, and Llama 4
entries are absent from the current serverless chat table and are no longer
retained as picker defaults. Users may still configure arbitrary Together
model IDs explicitly.

Sources:

- Together serverless model catalog:
  `https://docs.together.ai/docs/serverless/models`
- Together reasoning guide:
  `https://docs.together.ai/docs/inference/chat/reasoning`
- Together vision guide:
  `https://docs.together.ai/docs/inference/vision/overview`
- Together chat completions API:
  `https://docs.together.ai/reference/chat-completions-1`
