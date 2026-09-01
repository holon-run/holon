# RFC: Authenticated operator admission for low-authority prompts

## Status

Implemented with issue #2666.

## Contract

Holon separates **activation authority** from **content authority**:

- Activation authority comes from the authenticated ingress and its durable
  admission facts.
- Content authority comes from `MessageEnvelope::authority_class` and must not
  be upgraded during scheduling or execution admission.

An `OperatorPrompt` may therefore use `IntegrationSignal` or
`ExternalEvidence` content authority when all of the following hold:

- the message origin is `Operator`;
- a positive durable message sequence is present;
- the delivery surface and admission context are one of the authenticated
  operator pairs:
  `CliPrompt`/`LocalProcess`, `RunOnce`/`LocalProcess`,
  `HttpControlPrompt`/`ControlAuthenticated`, or
  `RemoteOperatorTransport`/`OperatorTransportAuthenticated`.

The scheduler uses this contract for both unbound operator prompts and prompts
bound to a WorkItem. The execution protocol keeps `ExecutionOrigin::Operator`
and maps the message's original `authority_class` to execution trust. In
particular, `trusted-integration` never becomes `OperatorInstruction` merely
because the prompt was submitted by an operator.

Missing sequence, mismatched surface/context, and non-operator origins remain
fail-closed. `trusted-system` is not expanded by this RFC; an operator CLI
submission cannot manufacture `RuntimeInstruction`.

## Replay and diagnostics

The positive message sequence is the durable ingress fence used by canonical
execution admission and survives message replay/restart. Queue-head invariant
diagnostics emitted while processing the normal run loop identify the
`run_loop` boundary; bootstrap recovery diagnostics retain their own boundary.
