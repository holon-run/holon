# Model route service tier

Status: accepted for the OpenAI and OpenAI Codex adapters.

Service tier is an endpoint request parameter, separate from model identity,
reasoning effort, scheduling priority, and credentials. It uses the existing
resolved endpoint `accepted_parameters` contract, projected to clients.

`model.route_options` stores defaults keyed by exact canonical
`provider@endpoint/model` routes. Agent overrides add optional `service_tier`
alongside the existing model and reasoning selection. Precedence is exact agent
route override, exact runtime route options, then unchanged adapter behavior.
Missing means inherit; `default` explicitly selects standard; `fast` requests
Fast. `priority` is accepted as an input alias and serialized as `fast`.

Only supported model IDs on official OpenAI/Codex endpoints advertise this
parameter initially. OpenAI-compatible transport alone does not enable it.
Unsupported explicit choices fail validation, without silently filtering the
chosen model out of the fallback chain. Each fallback resolves its own options;
an agent override applies only to its canonical selected route. Model switches
replace the previous route override. Changes take effect on the next turn.

Adapters send Fast as `service_tier: "priority"`. Explicit standard sends
`"default"` to the OpenAI API and omits the field for Codex, matching the Codex
client contract. Explicit Codex choices also set `x-codex-routing-hint` to
`model=<model>;tier=priority` for Fast or `model=<model>` for Standard, matching
the current Codex client routing contract. Missing preferences preserve the
legacy headers. Tier lowering happens before continuation planning so replay
and request shape validation preserve the setting. Request diagnostics record
requested wire tier and the independently reported served tier.

The Web GUI renders an independent speed selector using the backend capability
contract. Inherit, Standard, and Fast remain distinct; changing reasoning keeps
the route's speed override, while selecting another model drops it.

Fast can consume more quota or cost more. Account entitlement is checked by the
upstream service, and a requested tier is not proof of a served tier. No new
automatic downgrade or retry loop is introduced.

References: [OpenAI Fast mode](https://developers.openai.com/api/docs/guides/fast-mode),
[Codex speed](https://learn.chatgpt.com/docs/agent-configuration/speed),
[capability resolution](model-capability-resolution.md).
