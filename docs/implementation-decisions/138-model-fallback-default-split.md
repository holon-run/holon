# Model fallback default split (`auto` fallbacks, default never in the chain)

## Choice

The provider chain is `[primary] + fallback_models` where the primary is the
agent override, or `model.default` only when no override is set.
`model.default` is never inserted as an implicit fallback entry.
`model.fallbacks` entries are route refs or the `auto` marker; unset or empty
fallbacks default to `auto`, which expands at config-load time to
`authenticated_model_route_candidates` (one preferred route per authenticated
provider). The old logic that removed the default model (or its provider's
candidate) from the fallback list is gone; only exact duplicate suppression
remains, both within the fallback list and against the primary.

## Reason

`default_model` and `fallback_models` previously answered two overlapping
questions: with an override set, the default silently sat between the override
and the configured fallbacks, so the configured order was not honored and the
default's role changed depending on whether an override existed. Splitting the
two keys makes `model.default` mean "what to run when nothing more specific is
set" and `model.fallbacks` (with an explicit `auto` default) mean "how to
recover after a provider failure", matching the runtime-clarity-over-
convenience guardrail.

## Preserved boundary

`runtime.disable_provider_fallback: true` still collapses the chain to the
primary alone. Explicit fallback lists keep their configured order and no
longer drop entries equal to the default; when the default should act as a
safety net, list it in `model.fallbacks` (or use `auto`, whose candidates may
include the default route).
