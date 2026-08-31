# Explicit models.dev Provider Mapping

Decision:

- keep `models.dev` as a metadata/discovery source and add a versioned,
  Holon-owned mapping manifest before any provider offering can become a
  route candidate
- model provider, offering, route, and model identity as four separate
  layers; preserve upstream and Holon references in reports
- classify mappings as `direct`, `openai_compatible`, `gateway`, or
  `token_hub`, with exact provider identity and explicit route registration
- start Phase 3A with the existing `anthropic` provider and Anthropic Messages
  transport, then evaluate one verified OpenAI-compatible provider
- default new mappings to `enabled = false`; validation/reporting must not
  change the built-in catalog, preferred route, credentials, endpoint, or
  transport

Reason:

`models.dev` describes many offerings but does not establish that Holon has
the endpoint, credential, protocol implementation, or safe capability limits
needed to call them. A flat provider-to-provider alias would also conflate
direct providers, gateways, plan variants, and native protocols. The explicit
four-layer manifest makes discovery useful while preventing metadata from
silently becoming executable configuration.

Preserved boundary / tradeoff:

- capabilities and limits can only narrow through the intersection of upstream
  assertions, manifest ceilings, route support, and runtime policy
- missing upstream fields remain unknown rather than becoming `false`
- validation can report unmapped or disabled offerings without activating them
- remote refresh and native-provider adapters remain separate follow-up work
