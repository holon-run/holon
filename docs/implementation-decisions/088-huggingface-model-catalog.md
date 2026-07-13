# Hugging Face model catalog

Holon treats the Hugging Face OpenAI-compatible Router as a chat-only
aggregation endpoint. The built-in picker keeps only
`openai/gpt-oss-120b`, the current model used throughout the official
Inference Providers quick start. The previous unversioned
`moonshotai/Kimi-K2-Instruct` entry is removed because the current official
examples use other model ids and Router availability is dynamic.

The public `GET https://router.huggingface.co/v1/models` endpoint is the
authoritative discovery source. Holon keeps models with at least one provider
whose status is `live`, takes the largest published context length among live
routes, and projects image input from the model architecture. Provider tool
support does not establish portable parallel tool-call semantics, and the
Router response does not publish output limits or a general reasoning field,
so those values remain unset.

The official gpt-oss guide explicitly documents `low`, `medium`, and `high`
reasoning effort for the OpenAI-compatible Chat Completions API. Holon exposes
those controls only for the two gpt-oss model ids. Inference still requires
`HUGGINGFACE_API_KEY` or `HF_TOKEN`; public model discovery does not.

Sources reviewed on 2026-07-13:

- <https://huggingface.co/docs/inference-providers/index>
- <https://huggingface.co/docs/inference-providers/hub-api>
- <https://huggingface.co/docs/inference-providers/guides/gpt-oss>
- <https://huggingface.co/openai/gpt-oss-120b>
