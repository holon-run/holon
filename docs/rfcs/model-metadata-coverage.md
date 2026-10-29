---
title: RFC: Offline route metadata coverage and reviewed updates
date: 2026-10-25
status: accepted
---

# Offline route metadata coverage and reviewed updates

## Contract

Extend [metadata precedence](model-metadata-precedence.md) and
[models.dev mapping](models-dev-provider-mapping.md) without introducing
another resolver or making upstream data callable.

`models-dev coverage` reports the checked-in catalog, not local credentials,
discovery caches, or subscription eligibility. Every explicit route and every
otherwise unrepresented canonical model is resolved through the existing
field-level resolver. Aliases retain their canonical identity. Raw model and
route fields, effective facts, field origins, and upstream candidates remain
separate. Runtime output policy is not an assertion of output capacity.

Reviewed endpoint associations may declare `supported_models`: an exact
compatibility allowlist, not a rule generating default routes for every model.
These routes are enumerated independently of pinned or preferred routes and
must reference registered endpoints and existing canonical models. They do
not create runtime registrations or change preferred routes. The report retains
`required_routes`; the check rejects missing or duplicate coverage rows even
when the supplied review baseline agrees with an incomplete report.

Generic provider mappings apply to the default endpoint only. Reviewed
endpoint associations connect plan/region-specific upstream providers to exact
registered endpoints. Multiple upstream sources remain separate candidates;
they never overwrite each other or automatically populate the runtime catalog.
Unassociated plan providers remain visible as review work.
An association with `supported_models` compares only those exact IDs. Missing
upstream models are explicit review evidence, never an invitation to borrow
another endpoint's limits or capabilities.

## Review gate

`models.dev/metadata-review.json` records the accepted route facts. A changed
or newly added route must update this review baseline. Protected raw numeric,
capability and policy fields and resolver evidence are compared too: deleting
an explicit field requires review even when inheritance keeps its effective
value unchanged. Descriptive text is not part of this comparison. Context and output
capacity are independently `known`, `unknown`, `dynamic`, or `not_applicable`.
Zero is not a known capacity. Missing capacity requires a named owner, review
date, reason, evidence references, and a recheck condition. Dynamic and
not-applicable assessments never manufacture a numeric capacity.

The gate rejects unreviewed additions, changed facts (including known-to-empty
regressions), stale records, and undocumented unknowns. It runs offline against
checked-in inputs; upstream differences create candidates, not automatic
changes or network-dependent CI failures. Reviewing the baseline is part of
reviewing a catalog update, not an exemption from review.

Refresh reads the review baseline from its output directory and retains statuses
and reasons only while both facts and protected inputs still match. New or changed
draft routes remain unreviewed; refresh never writes an approval.

Existing builtin entries are still protected against supplement replacement.
To fill an existing field, review the exact endpoint/region evidence, edit the
Rust registration, regenerate the builtin snapshot, update the review record,
and run the existing snapshot and precedence tests. Never copy another
provider's limits merely because the model IDs match.

## Budget boundary

Unknown capacity remains unknown. An internal prompt budget is a local
execution policy, not a provider guarantee. Coverage records its resolved value
and requires positive prompt/output policies for agent-turn routes. Explicit
overrides, small known windows, and planner hard boundaries remain authoritative.
Live endpoint probes require separate authorization.
