# Ollama Tool-Only Round Placeholder User Text

Decision:

- keep the ollama `no user query found in messages` workaround inside the
  Anthropic-compatible transport, gated on `route_provider == "ollama"`
- when the whole request has no non-empty user text, append one placeholder
  text block `"(continue)"` to the last user message (or push a new user
  message when none exists) after cache marker computation
- do not generalize this into a provider quirk configuration
- remove the workaround once an upstream ollama release fixes
  ollama/ollama#18303 and passes local regression

Reason:

- ollama rejects pure tool-result turns (`assistant tool_use` followed by
  `user tool_result`) with HTTP 500, which makes agentic tool loops unusable
  against ollama models
- upstream fixes are unmerged and no release contains one, so a client-side
  shim is the only near-term path
- the placeholder lands after cache markers, so cache eligibility is unchanged

Boundary:

- only the ollama route is modified; real Anthropic and other compatible
  providers keep byte-identical request bodies
- the placeholder is visible to the model, so its text must stay a neutral
  continuation cue rather than an instruction
