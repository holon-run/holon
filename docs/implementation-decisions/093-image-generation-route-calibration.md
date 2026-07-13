# Image Generation Route Calibration

## Choice

Only models confirmed to accept the OpenAI Responses `image_generation` hosted
tool advertise Codex image generation. `gpt-5.3-codex-spark` does not advertise
that capability because the live Codex endpoint rejects the tool for that
model.

Volcengine Seedream uses the canonical `volcengine@default` route and Ark's
standard `/api/v3/images/generations` endpoint. The Agent Plan endpoint is not
an image-generation route.

## Reason

Image generation is both a model capability and a transport/endpoint contract.
Advertising either against an unsupported model or the wrong endpoint makes
automatic route selection deterministic but invalid.

## Preserved boundary

The legacy `volcengine-image-openai/doubao-seedream-5.0-lite` model ref and
`VOLCENGINE_IMAGE_OPENAI_API_KEY` remain accepted. They now normalize to the
standard Volcengine endpoint rather than preserving the historical incorrect
Agent Plan route.
