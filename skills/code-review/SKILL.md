---
name: code-review
description: "Review a set of code changes using evidence-backed findings, explicit confidence, and a clear coverage summary."
---

# Code Review Skill

## Summary

Use this skill to review a code change independently of the hosting platform.
The result is a concise, evidence-backed assessment for a human or another
workflow to consume. This skill does not know how to fetch a pull request or
publish comments.

## When To Use

- Reviewing a patch, commit range, change set, or proposed implementation
- Checking regressions before a merge, release, or deployment
- Producing findings for a platform-specific review adapter

## Do Not Use

- Implementing the fix while reviewing
- Approving, blocking, or otherwise replacing a human merge decision
- Treating an unavailable file, test, or instruction as evidence that no issue exists

## Inputs

Use the context supplied by the caller. It may include:

- `change_set`: changed files, hunks, and the before/after revisions
- `baseline`: relevant surrounding code, configuration, interfaces, and history
- `project_instructions`: repository or path-specific review rules
- `prior_feedback`: earlier findings and their current status
- `verification_budget`: commands or checks that may be run

Do not assume a particular directory or file name. If a caller supplies a
manifest, resolve inputs from its artifact entries and record missing or
unavailable entries in the coverage summary.

## Workflow

### 1. Establish scope and coverage

- Identify the exact change set and its baseline.
- Read applicable project instructions before judging behavior.
- List the files and hunks actually reviewed.
- Separate facts observed in the supplied context from assumptions.
- If core context is missing, continue only with an explicit limited-scope
  result; never infer a clean review from missing evidence.

### 2. Generate candidates

Review changed files and materially changed hunks in this order:

1. Correctness and data integrity
2. Security, authorization, trust boundaries, and secret handling
3. Lifecycle, failure handling, retries, cancellation, and cleanup
4. Concurrency, ordering, idempotency, and duplicate side effects
5. Compatibility, migrations, and public contract changes
6. Resource usage and performance
7. Tests, observability, and maintainability when they affect behavior

Follow important control flow into surrounding code when needed to establish
impact, but avoid speculative project-wide criticism.

### 3. Verify candidates

For every candidate finding:

- Re-read the relevant code and trace the behavior to a concrete outcome.
- Run a focused test, type check, lint, query, or other available verification
  when it can distinguish a real issue from a false positive.
- Confirm that the issue is introduced or materially worsened by the change.
- Confirm that the finding can be located precisely in the changed code.
- Lower confidence or omit the finding when evidence remains inconclusive.

Do not publish a high-severity finding based only on a pattern match, naming
preference, or an unverified hypothesis.

### 4. Classify and deduplicate

Keep only actionable findings supported by the change or directly relevant
surrounding code. For each finding, assign:

- `severity`: `critical`, `high`, `medium`, or `low`
- `confidence`: `confirmed`, `likely`, or `uncertain`
- `category`: the primary risk category
- `status`: `open`, `needs-context`, or `not-reproducible`

Do not report a finding as `critical` or `high` unless its evidence and impact
justify that severity. Merge duplicate findings and distinguish a new impact
from previously reported feedback.

### 5. Deliver the result

Return one user-facing brief containing:

1. Conclusion first: findings, clean review, or limited review
2. Findings ordered by severity, each with location, impact, evidence, and
   an actionable recommendation
3. Coverage: revisions, files/hunks examined, instructions available, and
   verification performed
4. Limitations and unresolved questions

If the caller explicitly provides an artifact directory and requests exports,
write `review.md` and `review-result.json` there. These are optional exports,
not required runtime files and not prerequisites for a valid review.

## Finding Contract

Represent each finding with this platform-neutral shape:

```json
{
  "id": "stable-within-this-review",
  "severity": "high",
  "confidence": "confirmed",
  "category": "correctness",
  "status": "open",
  "location": {
    "path": "src/example.rs",
    "start_line": 42,
    "end_line": 45
  },
  "title": "Short problem title",
  "impact": "Describe the user-visible or operational consequence.",
  "evidence": [
    "Describe the relevant code path, input, state transition, or verification."
  ],
  "recommendation": "Give a concrete direction for fixing or validating it.",
  "introduced_by_change": true
}
```

Use repository-relative paths and changed-line locations when available.
`location` may be omitted for a valid non-inline finding, but then explain why
the issue cannot be mapped precisely and keep it in the brief rather than
silently mapping it to an unrelated line.

## Degradation Rules

- Missing change-set or baseline context means `limited review`, not `clean`.
- Missing project instructions must be reported as an instruction-coverage
  limitation; do not claim that no such instructions exist.
- If verification cannot run, record the attempted command and reason.
- If a finding cannot be reproduced or precisely evidenced, mark it
  `needs-context` or omit it from actionable findings.
