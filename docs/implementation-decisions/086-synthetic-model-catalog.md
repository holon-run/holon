# Synthetic model catalog

Holon keeps Synthetic's four stable `syn:` aliases plus the currently
published always-on models in the built-in picker. The snapshot was verified
against the Synthetic Models page and public Models API on 2026-07-13.
Synthetic recommends the aliases because pinned `hf:` model names can be
rotated out; Holon therefore prefers `syn:large:text` and does not retain older
fixed models after they leave the always-on set.

Synthetic discovery uses the public
`https://api.synthetic.new/openai/v1/models` endpoint even though inference
continues to use the provider's documented Anthropic Messages transport.
Discovery includes only entries whose `always_on` value is explicitly `true`
and maps `context_length`, `max_output_length`, `input_modalities`, and
`supported_features` into remote route metadata.

The Models API exposes reasoning as a feature but does not publish a portable
discrete reasoning-effort vocabulary, so Holon marks reasoning capability
without inventing effort controls. Image input is enabled only when the API
lists the `image` input modality. Metadata refresh does not require a
configured Synthetic credential even though inference does.

Sources:

- Synthetic available models:
  `https://dev.synthetic.new/docs/api/models`
- Synthetic OpenAI Models API:
  `https://dev.synthetic.new/docs/openai/models`
- Synthetic Anthropic Messages API:
  `https://dev.synthetic.new/docs/anthropic/messages`
- Synthetic Models API:
  `https://api.synthetic.new/openai/v1/models`
