# BytePlus model catalog

Holon keeps BytePlus ModelArk's general and Coding Plan endpoints as built-in
provider routes, but does not currently ship static BytePlus model metadata.

BytePlus publishes a "Get Model ID" surface and separate ModelArk management
APIs for listing foundation models and versions. The publicly accessible
documentation does not provide a stable, complete model table from which Holon
can verify the current model IDs, lifecycle state, modalities, reasoning
controls, context windows, and output limits.

BytePlus metadata is therefore maintained independently from Volcengine Ark.
Matching product or model-family names are not evidence that the two endpoints
expose the same IDs or capabilities. Holon also does not claim that BytePlus's
OpenAI-compatible inference endpoints implement an OpenAI-style model-list
endpoint.

Users can still select a BytePlus model ID explicitly. Holon will resolve
unknown model limits through normal runtime fallback and user configuration
rather than presenting unverified built-in metadata.

Sources:

- BytePlus ModelArk model ID guide:
  `https://docs.byteplus.com/en/docs/modelark/model_id`
- BytePlus Seed 1.6 model documentation:
  `https://docs.byteplus.com/en/docs/modelark/1593702`
- BytePlus ModelArk API reference navigation, including
  `ListFoundationModels` and `ListFoundationModelVersions`:
  `https://docs.byteplus.com/en/docs/modelark`
