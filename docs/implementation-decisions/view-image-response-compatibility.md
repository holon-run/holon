# ViewImage response compatibility

`ViewImage` owns the canonical `visual_observation.v1` schema. The model-produced
`schema` field is compatibility metadata, not an authority over the internal
observation version: a structurally valid response with a missing or unknown
schema label is accepted and normalized to the tool-owned canonical value.

Local response validation remains strict for the JSON object, `type`, non-empty
`summary`, and supported array/object field shapes. If that validation fails,
`ViewImage` makes exactly one bounded correction retry using a prompt that
restates the expected JSON contract without including the untrusted response.
Provider-generation failures are not retried. A second validation failure keeps
the existing `vision_observation_failed` error code but reports
`failure_stage: response_validation` and bounded response previews.
