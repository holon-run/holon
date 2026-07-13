# Kilocode model catalog

Holon treats Kilocode as a dynamic OpenAI-compatible aggregation gateway. The
built-in catalog keeps the four current virtual routing models:
`kilo-auto/frontier`, `kilo-auto/balanced`, `kilo-auto/efficient`, and
`kilo-auto/free`. The previous `kilo/auto` entry is removed because it is not a
current model id in the official Gateway documentation or public model list.

The public `GET https://api.kilo.ai/api/gateway/models` endpoint is the
authoritative discovery source. Holon projects model ids, names, descriptions,
context windows, maximum completion tokens, image input, and reasoning support
from its structured fields. Tool support does not establish portable parallel
tool-call semantics. Reasoning effort controls are exposed only when the model
declares the `reasoning_effort` parameter and its OpenCode metadata publishes
standard effort-named variants.

The model directory can be refreshed without credentials, while inference
still requires `KILOCODE_API_KEY`. Static entries are intentionally limited to
the stable auto virtual models; the upstream provider catalog remains dynamic
and comes from discovery.

Sources reviewed on 2026-07-13:

- <https://kilo.ai/docs/gateway>
- <https://kilo.ai/docs/gateway/quickstart>
- <https://kilo.ai/docs/gateway/models-and-providers>
- <https://kilo.ai/docs/gateway/api-reference>
- <https://api.kilo.ai/api/gateway/models>

