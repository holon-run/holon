# Agent-Level Model Override

Decision:

- keep `model.default` and `model.fallbacks` as runtime-wide baseline
- allow one agent to override only its primary model
- keep inherited runtime defaults in the fallback chain

Reason:

- long-lived multi-agent runtime makes model choice an agent concern
- operators need per-agent comparison without perturbing the whole runtime
