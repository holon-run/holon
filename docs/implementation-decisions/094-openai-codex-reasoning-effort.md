# OpenAI Codex reasoning effort is model-specific

Holon uses each Codex model's catalog policy as the source of accepted
`reasoning_effort` values. Configuration parsing accepts the shared Codex
vocabulary through `max`, while provider construction and runtime model
overrides validate the value against the selected model.

`max` is exposed only for Codex models whose metadata declares it. `ultra` is
not exposed as an effort value because its upstream meaning includes execution
orchestration that Holon does not implement. If that behavior is added later,
it should be represented as an explicit execution capability rather than a
string passed through as ordinary reasoning effort.
