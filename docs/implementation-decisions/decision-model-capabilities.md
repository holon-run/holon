# Decision model capabilities and protocol routes

Decision reuses the shared provider registry and model catalog, but it does not
reuse the ordinary agent model view blindly.

- A model route must explicitly advertise `capabilities.decision: true` in its
  `models.catalog` override before `decision.model` can resolve.
- Ordinary agent selection requires `capabilities.agent_turn: true`.
  These flags are independent, so one provider may expose Turn-capable models,
  Decision-only models, or models supporting both.
- `decision_protocol` selects the adapter for that model route. `jev` is not
  inferred from provider name, transport, or model id; OpenAI-compatible routes
  must use an OpenAI-compatible transport.
- Provider endpoint and credential configuration remain shared. The Decision
  section stores only the selected route and runtime limits, avoiding a
  parallel provider/credential tree.

This preserves explicit trust and routing boundaries while allowing a provider
to serve mixed model families.
