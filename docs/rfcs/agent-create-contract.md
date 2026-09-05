# Canonical Agent Create Contract

Status: implementation baseline for Phase 0; the runtime service described here
is introduced by Phase 1.

This RFC defines the contract shared by `SpawnAgent` and future operator/control
plane creation entrypoints. It does not add a storage migration, an HTTP route,
or a new invocation surface.

## 1. Terms and invariants

- `agent_id` is the stable technical identity used by scheduling, permissions,
  task linkage, history, and lifecycle operations.
- `name` is a separate human-readable identity. Phase 0 defines its validation
  vectors only; it is not stored until Phase 2.
- `preset` selects the creation shape:
  - `private_child`: runtime-owned child with supervision/task linkage.
  - `public_named`: caller-selected stable identity intended for later direct
    addressing.
- A create request is not a message-delivery operation. `initial_message` may
  bootstrap a newly created private child, but it must never be applied to an
  existing identity or turn a duplicate create into an implicit invoke.
- `Active`, `Deleting`, and `Deleted` identities are all reserved. A deleted
  identity is a tombstone for identity allocation and must not be recreated.

The implementation must preserve these invariants even when the request is
retried, arrives concurrently, or fails after identity reservation.

## 2. Canonical request and trusted context

The domain request is the following logical shape:

```text
CreateAgentRequest {
  preset: private_child | public_named
  requested_agent_id?: string
  requested_name?: string       // Phase 2; validation contract only in Phase 0
  initial_message?: string
  template?: template reference
  workspace_mode?: inherit | worktree
  model?: model request
}

CreateCallerContext {
  origin: runtime/operator/channel provenance
  trust: authenticated authority classification
  authority: runtime-derived capability decision
}
```

`origin`, `trust`, and `authority` are not request-controlled fields. They are
attached by the trusted runtime boundary. JSON fields with those names must be
rejected by a strict transport schema or ignored before domain admission; they
must never elevate caller authority.

The current `SpawnAgent` tool maps `agent_id` to
`requested_agent_id`. Its `agent_id` requirement and preset-specific validation
remain the input boundary until the canonical service is introduced.

## 3. Field matrix

| Field | `private_child` | `public_named` | Rule |
| --- | --- | --- | --- |
| `preset` | optional, defaults to `private_child` | required when selecting public creation | Unknown values reject |
| `agent_id` | reject | required, non-empty, valid technical id | Never silently generate or reinterpret a supplied id |
| `initial_message` | required and non-empty for tool delegation | optional at request shape, but never delivered to an existing identity | No duplicate-create injection |
| `template` | supported subject to capability/authorization | supported subject to capability/authorization | Resolution is after reservation |
| `workspace_mode=inherit` | supported | supported | Default |
| `workspace_mode=worktree` | supported | reject as unsupported | Public identity cannot select an isolated child worktree |
| `model` | supported subject to capability/authorization | supported subject to capability/authorization | Resolution cannot mutate an existing profile |
| `name` | Phase 2 only | Phase 2 only | Separate from `agent_id`; conflict is explicit |
| `origin`/`trust`/`authority` | runtime context only | runtime context only | Never accepted from request JSON |

Whitespace-only optional strings normalize to absent before validation.
Technical ids are validated by the existing `validate_agent_id_format`
contract. Normalization must not cause two distinct supplied ids to become the
same identity without returning an explicit conflict.

## 4. Creation state machine

The canonical service executes these stages in order:

1. **Validate** request shape, preset combinations, ids, and capabilities that
   can be rejected without persistence.
2. **Authorize** using trusted caller context. Request data cannot raise
   `trust` or `authority`.
3. **Reserve technical identity** atomically. A reservation observes
   `Active`, `Deleting`, and `Deleted` records and never reuses any of them.
4. **Persist profile** with the selected preset and non-secret creation
   metadata.
5. **Resolve template, workspace, and model** against the reserved identity.
6. **Bootstrap**, if requested. Bootstrap is allowed only for the newly
   reserved identity.
7. **Publish a bounded receipt** after the committed state transition.

Reservation is the idempotency boundary. A concurrent or repeated request for
an already-reserved public id returns `already_exists` or
`identity_conflict`, according to whether the request is an exact replay or a
different create intent. It does not return the existing agent as a successful
new result, send `initial_message`, or replace its model/profile.

If a later stage fails, the receipt must identify the last committed stage and
the resulting lifecycle state. Retry behavior is stage-specific: a caller may
retry a failed resolution/bootstrap operation only through an explicit repair
or retry contract added after Phase 0; it must not retry by issuing an
unqualified create against the same identity.

## 5. Typed errors

The service-level error taxonomy is stable even when transport mappings differ:

| Code | Meaning | Required side effect |
| --- | --- | --- |
| `invalid_request` | malformed value or unsupported field combination | No identity reservation |
| `unauthorized` | caller lacks the required trusted authority | No identity reservation |
| `already_exists` | exact public identity already exists | No bootstrap, message, or profile mutation |
| `identity_conflict` | requested id/name conflicts with a different intent | No mutation of the existing identity |
| `lifecycle_conflict` | identity is deleting, deleted, or otherwise fenced | Identity remains reserved |
| `unsupported_capability` | template, workspace, or model capability is unavailable | Receipt reports whether reservation committed |
| `bootstrap_failed` | profile was created but bootstrap did not complete | Receipt reports committed identity and failure stage |

Transport errors may wrap these codes, but callers must not have to parse
human-readable error strings to distinguish them.

## 6. Receipt contract

Every admitted create attempt returns or persists a bounded receipt containing:

```text
CreateReceipt {
  receipt_id
  agent_id?: technical identity, if reserved
  preset
  stage: validated | reserved | profiled | resolved | bootstrapped | failed
  lifecycle: active | deleting | deleted | failed
  task_id?: private-child supervision linkage
  work_item_id?: linkage when one exists
  model_summary?: provider/model/capability summary without credentials
  template_summary?: stable template reference/version without secret contents
  workspace_summary?: mode and execution-root summary without paths/secrets
  error_code?: typed service error
}
```

Receipt fields are additive and machine-readable. `initial_message`, prompt
contents, provider credentials, workspace secrets, and task trace are never
included. A failure receipt must still be safe to log and expose to an
authorized caller.

## 7. Name contract reserved for Phase 2

`name` is a display identity, not an allocation key for scheduling. The Phase 2
implementation must use one namespace per owner/runtime scope, trim surrounding
whitespace, apply Unicode-aware case folding for uniqueness, and reject empty,
overlong, control-character, and separator-containing values. The exact maximum
length and allowed character vector must be implemented from the tests below,
not inferred from UI rendering.

Create and rename use an atomic uniqueness check. A conflict returns
`identity_conflict`; it never silently changes the requested name. Rename does
not change `agent_id`, owner, authority, task target, or historical references.
Names associated with `Deleting` or `Deleted` identities remain unavailable
until the lifecycle/tombstone policy explicitly releases them; technical ids
are never reused.

## 8. Phase 0 executable test vectors

The following vectors are the minimum baseline for the canonical service and
transport adapters:

| Vector | Input | Expected result |
| --- | --- | --- |
| private success | `private_child` + non-empty `initial_message` | New private identity and supervision linkage |
| public success | `public_named` + valid `agent_id` | New public identity |
| duplicate public | same valid public id twice | Second request is `already_exists`; no second bootstrap/message/model mutation |
| private missing message | `private_child` without message | `invalid_request`; no reservation |
| private technical id | private request with `agent_id` | `invalid_request`; no reservation |
| public missing id | `public_named` without id | `invalid_request`; no reservation |
| public invalid id | empty or invalid-format id | `invalid_request`; no reservation |
| public worktree | `public_named` + `workspace_mode=worktree` | `unsupported_capability`/transport invalid input; no reservation |
| forged provenance | request supplies `origin`, `trust`, or `authority` | Strict schema rejects or trusted context wins; no elevation |
| lifecycle fence | id is `Active`, `Deleting`, or `Deleted` | `already_exists` or `lifecycle_conflict`; never recreate |
| bootstrap failure | failure after reservation | `bootstrap_failed` receipt identifies reserved identity/stage and omits message |
| name normalization | equivalent normalized names in one namespace | First succeeds, second is `identity_conflict` |
| tombstone name | name belongs to deleted/tombstoned identity | Conflict; no silent reuse |

These vectors are contract tests, not permission to add storage or endpoint
behavior in Phase 0.
