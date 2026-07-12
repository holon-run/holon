# 068 Gemini Model Catalog

## Choice

Keep the built-in Gemini catalog limited to current general-purpose
`generateContent` model codes documented by the Gemini API:

- `gemini-3.5-flash`
- `gemini-3.1-pro-preview`
- `gemini-3.1-flash-lite`
- `gemini-2.5-pro`
- `gemini-2.5-flash`
- `gemini-2.5-flash-lite`

Remove the undocumented `gemini-3-pro` and `gemini-3-flash` aliases. Prefer the
stable `gemini-3.5-flash` route over the preview Pro route.

The catalog records image input and thinking as intrinsic model capabilities.
The current `gemini_generate_content` transport does not lower image input or a
thinking control, so resolved routes must not advertise vision observation or
`reasoning_effort` until those transport features are implemented.

## Reason

Google's model pages document the listed model codes with 1,048,576 input
tokens, 65,536 output tokens, multimodal input, function calling, and thinking.
The previous Gemini 3 names were product-family labels rather than current API
model codes and could produce invalid requests.

This change deliberately does not add Gemini OAuth or image-generation models.
Those require separate authentication and transport decisions rather than
catalog-only metadata.

## Sources

Verified 2026-07-12:

- <https://ai.google.dev/gemini-api/docs/models/gemini-3.5-flash>
- <https://ai.google.dev/gemini-api/docs/models/gemini-3.1-pro-preview>
- <https://ai.google.dev/gemini-api/docs/models/gemini-3.1-flash-lite>
- <https://ai.google.dev/gemini-api/docs/models/gemini-2.5-pro>
- <https://ai.google.dev/gemini-api/docs/models/gemini-2.5-flash>
- <https://ai.google.dev/gemini-api/docs/models/gemini-2.5-flash-lite>
- <https://ai.google.dev/gemini-api/docs/deprecations>
