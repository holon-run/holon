# Qianfan model catalog

Holon's `qianfan` provider uses Baidu AI Cloud Qianfan's OpenAI-compatible V2
Chat Completions endpoint at `https://qianfan.baidubce.com/v2`.

The built-in catalog records a conservative current subset of model IDs exposed
by Qianfan's official V2 model listing: ERNIE 5.0, ERNIE 5.1, ERNIE X1.1, and
DeepSeek V3.2 variants whose limits and modalities are explicitly documented.
It does not retain historical ERNIE IDs merely because they appeared in older
release notes.

Qianfan publishes separate `context_length`, prompt, and completion limits.
Holon's `context_window_tokens` is the total context window, so the catalog uses
`context_length` rather than the smaller prompt-only limit. Output limits use
the documented maximum completion-token value.

Thinking and X1 model IDs are marked as fixed reasoning models. This does not
claim that Qianfan accepts OpenAI's `reasoning_effort` parameter; no reasoning
effort options are projected for Qianfan. Standard model IDs remain
non-reasoning unless the official model metadata explicitly identifies them as
thinking models.

Sources:

- Qianfan V2 API and model list:
  `https://cloud.baidu.com/doc/qianfan-api/s/Dmba8k71y`
- Qianfan model documentation:
  `https://cloud.baidu.com/doc/qianfan/s/qmh4sv5vi`
- Qianfan model retirement history:
  `https://cloud.baidu.com/doc/qianfan/s/Kmh4stnjp`
- Qianfan Responses API:
  `https://cloud.baidu.com/doc/qianfan-api/s/vmhejnuy8`
