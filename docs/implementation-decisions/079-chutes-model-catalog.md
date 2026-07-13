# Chutes model catalog

Holon snapshots Chutes's public live model directory for its built-in model
picker. Chutes documents unauthenticated `GET /v1/models` as the source of truth
for model IDs, context and output limits, modalities, supported features, and
confidential-compute status.

The snapshot was verified on 2026-07-13. Holon records `reasoning` as a fixed
model capability because the directory advertises the feature but does not
publish an adjustable reasoning-effort parameter. Image input is enabled only
when the Chutes entry explicitly lists the `image` input modality. Video input
is not represented because Holon's current model capability contract has no
video modality.

The live `unsloth/Mistral-Nemo-Instruct-2407-TEE` entry publishes only its
context length and confidential-compute flag. Holon therefore keeps it with
conservative metadata: no inferred reasoning or image capability and no
claimed output limit.

Chutes is a dynamic aggregation endpoint. Models no longer present in the
public live directory are removed from the built-in picker rather than retained
from upstream model-family assumptions. Users may still configure arbitrary
Chutes model IDs explicitly.

Sources:

- Chutes LLM-friendly documentation index:
  `https://chutes.ai/llms.txt`
- Chutes public live model directory:
  `https://llm.chutes.ai/v1/models`
