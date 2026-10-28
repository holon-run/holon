---
title: Models, providers, and routing
summary: Model capabilities, executable provider routes, agent overrides, fallback, Fast mode, and advisory decisions.
order: 110
---

# Models, providers, and routing

A model supplies capabilities such as text generation or image understanding.
A provider supplies access to models through a configured service or local
endpoint. A route selects the provider endpoint through which Holon will call
a model.

This distinction matters when the same model is available through different
services. The model name alone does not establish which credentials, endpoint,
or service options the request will use.

## Availability and capabilities

The model catalog describes models and their capabilities. An executable route
also needs a usable provider configuration and, where required, credentials.
Catalog presence does not prove that a route is available to this runtime.

Check capabilities for the intended operation. A conversation model may not
support image input, image generation, or the Decision subsystem. A text task
working successfully does not prove those other operations are configured.
The [model reference](../reference/models.md) is the catalog entry point;
[configuration](../reference/configuration.md) describes availability and
credential inspection.

## Global defaults and agent choices

The global default supplies the conversation route for agents without an
override. An agent-level selection overrides that primary route for the
agent without changing the global default.

For example, a documentation agent and a code-review agent can use different
primary routes while sharing the same runtime. Changing one agent's model
does not give it different permissions or transfer its work to a new identity.

Holon distinguishes a logical model reference from an executable route that
also names an endpoint. Use the route required by the setting rather than
assuming every model identifier is interchangeable.

## Fallback and dedicated routes

The conversation provider chain starts with the agent override, or the global
default when no override exists, followed by configured fallbacks. The global
default is not silently added as a fallback after an agent override. Automatic
fallback selection can use preferred routes from authenticated providers.

Fallback improves availability but can change the provider that receives a
request. Inspect or disable it when a task requires a specific provider.
It does not make providers equivalent in capabilities or output, and it does
not guarantee recovery from every failure.

Image inspection, image generation, and advisory decisions have their own
routing requirements. Configuring a conversation model is not a complete
configuration for all tools. Dedicated vision and image-generation selections
let those operations use suitable routes without replacing the conversation
model.

## Provider admission, cooldown, and resilience

Holon coordinates provider dispatch through pre-turn admission, persistent
request budgets, and shared cooldown management:

- **Whole-chain admission and budgets** — Candidate provider chains and token
  budgets are validated before dispatch begins, preventing partial dispatch
  storms and unrecoverable budget failures.
- **Shared cooldowns and same-turn tool continuation** — When an endpoint is
  temporarily rate-limited or degraded, Holon applies an isolated shared
  cooldown. Tools scheduled in the current turn keep running so long-running
  turns can finish active tool calls before waiting or falling back.
- **Candidate isolation and rate-limit persistence** — Routes returning 402
  (Payment Required) are persistently isolated from immediate retry loops and
  re-evaluated only through unified recovery. Quota-less 429 rate limits persist
  across runtime restarts to avoid repeated threshold collisions.

## Fast mode

On supported routes, Fast mode requests a faster service tier. It is separate
from the model's thinking level and can consume more quota or cost more.
Availability depends on the exact route; a compatible gateway does not
automatically inherit the service's Fast capability.

Auto uses the route default. Standard can override a Fast default. Fallback
routes use their own defaults, and an upstream service may serve a different
tier from the requested one. Treat Fast as a request, not a latency guarantee;
inspect requested and served tiers when diagnosing it.

## Advisory decisions

The optional Decision subsystem can supply a bounded second opinion among
specified choices. For example, an agent might ask which of several diagnostic
checks to try first. The answer is evidence for the agent to consider.

It cannot grant permission, satisfy an approval requirement, or change task
lifecycle state. Limits, timeout, provider failure, invalid choices, or
insufficient confidence can produce an explicit abstention. The caller must
handle that outcome without treating it as approval.

## Next steps

- [Configure models, credentials, and fallback](../guides/configure-models.md)
- [Configure advisory decisions](../guides/configure-decision-subsystem.md)
- [Model catalog and route options](../reference/models.md)
- [Configuration reference](../reference/configuration.md)
