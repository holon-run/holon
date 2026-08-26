# Reasoning effort preserves its configuration scope

Agent model override `reasoning_effort` belongs to the exact model route named
by that override. Provider construction applies it only to that resolved
candidate, so later candidates in the model fallback chain do not inherit a
value selected for another model.

An explicit `reasoning_effort` in provider endpoint configuration retains its
endpoint-wide scope. Every candidate using that endpoint validates the value
against its own model policy; incompatible candidates are excluded, and
provider construction fails if no candidate remains.

Holon does not translate effort values between models. Fallback changes the
active model route, not the meaning or ownership of a model-specific override.
