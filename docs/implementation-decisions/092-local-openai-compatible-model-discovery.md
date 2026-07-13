# Local OpenAI-Compatible Model Discovery

LiteLLM Proxy and vLLM are deployment surfaces, not hosted catalogs with a
stable Holon-chosen model. Their configured model ids or aliases are determined
by the running server.

Holon therefore keeps built-in endpoint and authentication defaults for these
providers but does not ship a representative static model. Both providers use
their OpenAI-compatible `GET /v1/models` endpoint to populate the discovery
cache. LiteLLM exposes both `/models` and `/v1/models`; Holon uses the versioned
route consistently. Discovered ids receive conservative unknown-model policy
and no inferred capabilities or token limits because the standard model-list
shape does not describe those properties.

LiteLLM keeps `LITELLM_API_KEY` bearer authentication because a proxy configured
with a master key protects its OpenAI-compatible routes. vLLM remains
unauthenticated by default; operators can override provider authentication when
starting vLLM with `--api-key` or `VLLM_API_KEY`.

Primary references:

- LiteLLM Proxy quick start: <https://docs.litellm.ai/docs/proxy/quick_start>
- LiteLLM Proxy route inventory:
  <https://github.com/BerriAI/litellm/blob/main/litellm/proxy/README.md>
- vLLM OpenAI-compatible server:
  <https://docs.vllm.ai/en/latest/serving/openai_compatible_server.html>
- vLLM security and API-key scope:
  <https://docs.vllm.ai/en/latest/usage/security.html#api-key-authentication-limitations>
