# case-name

## Purpose

Describe the end-to-end behavior being validated.

## Preconditions

- Required repositories, secrets, tokens, and branches.
- Required workflow references.

## Steps

1. Prepare the live target.
2. Trigger Holon.
3. Wait for completion.
4. Validate expected side effects.
5. Collect evidence.

## Expected

- Expected workflow result.
- Expected GitHub side effects.

## Pass / Fail Criteria

- Pass:
  - Workflow exits successfully.
  - Required artifact exists.
  - Required side effect is visible on GitHub.
- Fail (`infra-fail`):
  - Auth, workflow, checkout, model, runtime, or token broker failure.
- Fail (`agent-fail`):
  - Runtime completes but expected agent behavior is missing.

## Evidence to Capture

- Workflow run URL.
- Target issue/PR URL.
- `holon-solve-output` artifact.
- Relevant GitHub side-effect URLs.
