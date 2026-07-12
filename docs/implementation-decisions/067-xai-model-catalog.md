# xAI Model Catalog

## Choice

The built-in `xai` catalog follows xAI's current recommended general-purpose
models:

- `grok-4.3`
- `grok-4.5`

Grok 4.3 uses a 1M-token context window and accepts text and image input. It
supports `none`, `low`, `medium`, and `high` reasoning effort. Grok 4.5 uses a
500k-token context window, accepts text and image input, and supports `low`,
`medium`, and `high` reasoning effort; reasoning cannot be disabled.

xAI's current model pages do not publish a maximum output-token limit for
either model. The catalog therefore does not invent an upper limit and leaves
the runtime output budget controlled by Holon's configured default.

## Reason

This list was verified against xAI's official model pages, reasoning guide, and
May 15 retirement guide on 2026-07-12.

The retirement guide states that `grok-3`, `grok-4-0709`,
`grok-4-fast-reasoning`, `grok-4-fast-non-reasoning`,
`grok-4-1-fast-reasoning`, `grok-4-1-fast-non-reasoning`, and
`grok-code-fast-1` were retired on 2026-05-15. Requests to most retired slugs
are redirects to Grok 4.3 rather than distinct model contracts. Early Grok 3
and Grok 4 variants that are absent from the current model surface are also
omitted from the conservative built-in catalog.

## Migration

Users pinning an omitted xAI model should select `xai/grok-4.3` or
`xai/grok-4.5`. An omitted slug remains addressable as an unknown model but
uses fallback metadata instead of calibrated context, image, reasoning, and
compaction behavior.

## Preserved boundary

Grok Build and Imagine use distinct product or API contracts and are not added
to the ordinary text-model catalog by this calibration.
