---
name: holon-runtime-ops
description: "Operate and diagnose Holon runtimes with metadata-only patrol reports, incremental error analysis, and sanitized bug escalation."
---

# Holon Runtime Operations Skill

## Summary

Use this skill to inspect Holon runtime health, diagnose agent and lifecycle
failures, maintain incremental error checkpoints, produce explicitly
authorized patrol reports, and prepare sanitized upstream bug reports.

This skill extends the platform-neutral `ops` workflow. It does not grant
credentials, remediation authority, scheduled execution, direct database
access, or GitHub publication authority.

## When To Use

- diagnosing Holon daemon, agent, WorkItem, task, wait, timer, event-ingress,
  provider, tool, workspace, or delivery behavior
- preparing an authorized Holon restart, upgrade, rollback, or recovery
- counting and deduplicating new Holon runtime errors
- running an explicitly configured report-only patrol
- preparing or publishing a sanitized `holon-run/holon` issue under the
  applicable bug-reporting policy

## Do Not Use

- for general server or service administration unrelated to Holon
- to read task objectives, messages, prompts, transcripts, memory, or model
  request bodies for an activity report
- to turn a diagnosis or patrol authorization into repair authority
- to access the runtime database by default or depend on a private schema
- to publish security, privacy, credential, personal-data, or corruption
  findings automatically

## Authority Gates

Treat these as separate authorizations:

1. read-only runtime diagnosis
2. maintenance or remediation
3. scheduled patrol
4. anomaly notification
5. direct runtime-database read
6. direct runtime-database write
7. GitHub duplicate search
8. issue draft creation
9. issue publication

Record the scope, source, expiry, and revocation conditions of standing
authorizations. If a gate is absent or ambiguous, stop at the last authorized
step and report what is needed next.

## Source Priority

Collect evidence in this order:

1. native runtime tools for their declared responsibility
2. declared machine-readable `holon` CLI commands
3. authoritative service or deployment logs
4. deployment configuration and version metadata
5. separately authorized read-only runtime-database queries as a final
   deep-diagnostic fallback

Use `holon commands` to discover the current CLI contract and `holon context`
to inspect caller provenance. Do not manually construct caller-context
environment variables and do not use recursive `holon run` or `holon prompt`
as a control-plane substitute.

Direct database writes are not a diagnostic technique. They are H3 recovery
actions requiring exact approval, a snapshot, a bounded mutation, rollback,
and verification.

## Installation Inventory

Keep one record per Holon installation:

```text
work/inventory/installations/<installation-id>/info.yaml
```

Recommended fields:

```yaml
schema: holon.ops.installation.v1
id: local-dev
environment: development
owner: operator
deployment_mode: local-binary
version:
  reported: 0.1.0
  commit: "<git-sha-or-null>"
runtime:
  endpoint_ref: local-control-plane
  service_ref: null
authoritative_sources:
  - kind: runtime
    ref: native-tools
  - kind: deployment
    ref: "<safe-reference>"
data_boundaries:
  activity_reports: runtime-metadata-only
  database_access: disabled
notes: null
```

Do not place credentials, callback URLs, secret values, private keys, or raw
payloads in inventory.

## Standard Diagnosis

1. **Identify** — installation, environment, version, deployment mode, impact,
   reporting window, and authority.
2. **Snapshot** — current agent/runtime state and deployment state using the
   highest-priority available sources.
3. **Bound** — choose the smallest relevant agents, WorkItems, tasks, errors,
   components, and time range.
4. **Collect** — lifecycle metadata, statuses, timestamps, exit classes,
   redacted errors, and authoritative log references.
5. **Correlate** — order evidence by time and explicit state transitions.
6. **Classify** — expected behavior, configuration, dependency, deployment,
   suspected Holon defect, security/privacy, or unknown.
7. **Report** — facts, inference, confidence, impact, workaround, missing
   evidence, and recommended next action.
8. **Change only if authorized** — use the `ops` preflight, execution, rollback,
   operation-record, and verification workflow.

Do not treat queued, yielded, blocked, waiting, and current WorkItems as
interchangeable. Use the runtime's declared lifecycle views rather than
reconstructing scheduler state from incidental logs.

## Incremental Error Analysis

Maintain one checkpoint per installation and source:

```text
work/checkpoints/<installation-id>-<source-id>.json
```

Example:

```json
{
  "schema": "holon.ops.checkpoint.v1",
  "source": "runtime-events",
  "cursor_kind": "timestamp_and_id",
  "cursor": {
    "timestamp": "2026-09-06T00:00:00Z",
    "id": "event-redacted"
  },
  "last_successful_run": "2026-09-06T00:10:00Z"
}
```

Rules:

- use a deterministic boundary such as `(timestamp, stable_id)` when possible
- query with overlap, then deduplicate, so equal timestamps and late records
  are not lost
- fingerprint from sanitized component, error class, normalized message shape,
  relevant state transition, and version; never hash a secret-bearing raw body
  and publish the hash as if it were safe
- distinguish occurrence count, affected agents or operations, first seen,
  last seen, and new-versus-known status
- state explicitly when sources are missing, truncated, reset, or inconsistent
- write the new checkpoint only after collection, analysis, report persistence,
  and required delivery all succeed
- on failure, retain the previous checkpoint and record the failed run

## Patrol Configuration

Patrol is disabled until the operator approves and persists a policy:

```text
work/policies/patrol.yaml
```

Suggested contract:

```yaml
schema: holon.ops.patrol.v1
enabled: false
mode: report-only
timezone: Etc/UTC
schedule:
  kind: daily
  at: "09:00"
scope:
  installations: []
  environments: []
  agents: []
  exclude_agents: []
window:
  kind: since-last-success
checks: []
data_policy: runtime-metadata-only
notifications:
  report_destination: null
  anomaly_destination: null
  silence_windows: []
review:
  expires_at: null
  next_review_at: null
```

Confirm the IANA timezone, daylight-saving behavior, scope, exclusions,
reporting window, checks, timeout, concurrency, destinations, retention,
checkpoint initialization, review date, and revocation conditions before
scheduling.

An enabled patrol remains report-only unless a separate named remediation
authorization exists. Never infer automatic repair, restart, cancellation,
upgrade, issue submission, or database access from the patrol policy.

## Metadata-Only Activity Reports

Approved report data may include:

- installation and runtime version
- agent ID, lifecycle state, creation time, last approved activity timestamp,
  and state transition counts
- aggregate WorkItem, task, wait, timer, and delivery counts or outcomes
- newly observed error fingerprints and trends
- recorded deployments, restarts, maintenance actions, findings, and incidents

Excluded data:

- WorkItem objective or plan content
- task command or prompt content
- operator, external, or model messages
- prompts, transcripts, memory, briefs, and model request or response bodies
- tool input/output payload bodies unless separately needed for a diagnosis and
  excluded from the report
- environment values, credentials, callback capabilities, and secret-manager
  material

Define “active” in the policy using approved metadata signals, for example a
lifecycle transition, task start/completion, or runtime-recorded activity
timestamp inside the report window. Do not infer activity from content access.

Recommended report outline:

```markdown
# Holon daily operations report
## Executive summary
## Runtime and daemon health
## Agent current-state distribution
## Agent activity in reporting window
## WorkItem, task, wait, timer, and delivery lifecycle health
## New errors and trend versus previous window
## Open findings and incidents
## Deployments, restarts, and maintenance actions
## Recommended operator actions
## Data coverage and limitations
```

Persist reports under `work/reports/daily/YYYY-MM-DD.md`. Include the exact
timezone, UTC bounds, policy revision, sources, excluded sources, and
checkpoint outcome.

## Finding and Incident Records

Use findings for bounded observations that need tracking:

```text
work/findings/FIND-<timestamp>-<slug>.md
```

Include installation, time window, affected component and version, sanitized
evidence, fingerprint, occurrence count, impact, confidence, classification,
workaround, missing evidence, and recommended action.

Use incidents for active or materially impactful operational events:

```text
work/incidents/INC-<timestamp>-<slug>.md
```

Follow the `ops` incident and operation-record rules. Link findings, reports,
authorized operations, and issue drafts instead of duplicating their full
contents.

## Bug Escalation Policy

Persist the policy at:

```text
work/policies/bug-reporting.yaml
```

Suggested contract:

```yaml
schema: holon.ops.bug-reporting.v1
mode: disabled
repository: holon-run/holon
allowed_categories: []
rate_limit:
  max_issues: 0
  per_hours: 24
expires_at: null
require_duplicate_search: true
forbidden_auto_submit:
  - security
  - privacy
  - credential
  - personal-data
  - data-loss
```

Modes:

- `disabled`: local findings only
- `draft-only`: create a sanitized draft and wait for operator review
- `scoped-submit`: publish only while an explicit repository-, category-,
  duration-, frequency-, and revocation-bounded policy is active

First enablement must be `draft-only`. This skill defines `scoped-submit` for a
possible future authorization but grants none.

## Sanitized Issue Workflow

1. Confirm the bug-reporting mode and publication authority.
2. Establish affected version, environment class, frequency, impact, and a
   minimal reproducer or strong evidence chain.
3. When GitHub read access is authorized, search open and closed
   `holon-run/holon` issues for duplicates.
4. Classify security, privacy, credential, personal-data, corruption, or
   data-loss findings for private escalation; do not auto-publish them.
5. Replace identifying or secret-bearing values with stable placeholders.
6. Write the issue body to
   `work/issue-drafts/<timestamp>-<slug>.md`.
7. Review the rendered body and every attachment for leakage.
8. In `draft-only`, stop and request operator approval.
9. In `scoped-submit`, revalidate repository, category, expiry, rate limit,
   revocation, duplicate result, and forbidden categories immediately before
   publication.
10. Publish using `ghx` file-based payload guidance and record the resulting
    issue reference without copying authentication data.

Sanitize at least:

- installation, agent, WorkItem, task, event, request, and provider IDs
- usernames, home directories, absolute paths, hostnames, IP addresses, and
  internal repository or branch names
- callback URLs and capability-bearing query strings
- tokens, cookies, headers, environment values, secrets, and key material
- message, prompt, transcript, memory, objective, brief, and model payload
  content

Do not assume hashing makes sensitive data publishable. Prefer descriptive
placeholders such as `<agent-id>`, `<local-path>`, and `<request-id>`.

Recommended issue outline:

```markdown
## Summary
## Environment and Holon version
## Minimal reproduction
## Expected behavior
## Actual behavior
## Sanitized evidence
## Impact and frequency
## Workaround
## Duplicate search
## Additional context
```

## Completion Criteria

A diagnosis or patrol run is complete only when:

- scope, authorization, sources, and data limitations are recorded
- observations and inferences are separated
- errors are deduplicated and checkpoint handling is explicit
- the report or finding is persisted and delivered as configured
- no excluded content or secret-bearing material was retained
- any change has an `ops` operation record, rollback result, and verification
- issue publication either stopped at a draft or recorded the exact applicable
  publication authority

If any criterion is unmet, report partial completion and do not advance the
checkpoint.
