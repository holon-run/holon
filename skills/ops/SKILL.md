---
name: ops
description: "Operate servers and services with read-only-first diagnosis, explicit authorization, auditable inventory, operation records, rollback, and verification."
---

# Operations

Use this skill for platform-neutral server and service operations. It defines
the workflow and record formats; environment-specific skills and tools own
Linux, SSH, containers, Kubernetes, IaC, cloud, and observability commands.

## Core rules

1. Identify the exact environment, hosts, services, source of truth, and
   operator request.
2. Determine the authorization level before touching an external system:
   - L0: local inventory and documentation only.
   - L1: read-only checks within an explicitly confirmed scope.
   - L2: reversible changes, requiring per-action approval unless covered by a
     scoped, expiring, revocable runbook authorization.
   - L3: disruptive, privileged, network, IAM, DNS, data, secret, bulk, or
     irreversible work, requiring confirmation of the exact action.
3. Prefer read-only discovery and preflight checks. Do not infer repair
   authority from diagnosis or urgency.
4. Before a change, state targets, expected effect, risk, commands or API
   actions, success criteria, rollback trigger, and rollback steps.
5. Use a canary and bounded batches for multi-resource work. Verify each batch
   before proceeding.
6. Record actual actions, sanitized evidence, results, and deviations.
7. Verify service health and intended state after the action. Roll back only
   when authorized or when a confirmed runbook explicitly permits it.

Stop and ask when target identity, current state, authority, impact, rollback,
or verification is unclear.

## Inventory layout

Create only the files needed for the current environment:

```text
work/
  inventory/
    sources.md
    hosts/<host-id>/info.yaml
    hosts/<host-id>/operations.md
    services/<service-id>/info.yaml
    services/<service-id>/operations.md
  runbooks/
  inspections/
  operations/YYYY/YYYY-MM/
  incidents/
```

`sources.md` records each authoritative source, owner, refresh method, and last
confirmation. Prefer external sources of truth. AgentHome inventory is a cache
unless the operator explicitly designates it authoritative.

Use stable IDs and `schema_version: 1`. A host `info.yaml` should contain:

```yaml
schema_version: 1
id: prod-web-01
display_name: Production Web 01
environment: production
status: active
owners: [platform]
criticality: high
location: {provider: example-cloud, region: us-east-1}
roles: [web]
access:
  ssh_config_alias: prod-web-01
  bastion_alias: prod-bastion
  credential_ref: secret-manager://ops/prod-web
platform: {os: ubuntu, architecture: amd64}
services: [customer-web]
monitoring: {dashboards: [], alerts: []}
source:
  kind: cmdb
  ref: host/prod-web-01
  last_synced_at: 2026-09-06T00:00:00Z
last_confirmed_at: 2026-09-06T00:00:00Z
```

A service `info.yaml` should contain:

```yaml
schema_version: 1
id: customer-web
display_name: Customer Web
environment: production
status: active
owners: [web-team]
criticality: high
service_type: systemd
runs_on: {hosts: [prod-web-01, prod-web-02]}
dependencies: [customer-api]
repository: github:example/customer-web
deployment_source: gitops:environments/prod/customer-web
runbook: work/runbooks/customer-web.md
monitoring: {dashboards: [], alerts: []}
backup: {policy_ref: backup/customer-web}
maintenance:
  timezone: America/New_York
  windows: []
source:
  kind: gitops
  ref: services/customer-web
  last_synced_at: 2026-09-06T00:00:00Z
last_confirmed_at: 2026-09-06T00:00:00Z
```

Never store a credential value. `credential_ref` may contain only a controlled
secret-manager reference or local connection alias. Keep permission policies
separate from resource facts so an inventory edit cannot expand authority.

When renaming or retiring a resource, preserve its stable ID through a
redirect or tombstone so historical links remain valid.

## Operation records

Create one authoritative record per logical operation:

```text
work/operations/YYYY/YYYY-MM/OP-<UTC timestamp>-<slug>.md
```

Record:

- UTC time and operator-local time with IANA timezone
- WorkItem, incident, change, or request reference
- actor, target environment, host IDs, and service IDs
- request and authorization summary, including standing authorization scope
- purpose, risk level, preflight results, plan, and rollback plan
- actual commands or API actions, with secrets and sensitive output redacted
- exit status, changed resources, verification evidence, and final result
- rollback status, residual risk, follow-up owner, and due condition

Do not silently rewrite an error. Append a dated correction. Put large raw
output in a controlled external log or attachment and retain only a summary
and reference.

Append a concise link to each affected resource's `operations.md`:

```markdown
## 2026-09

- `2026-09-06T10:30:00Z` · change · success ·
  [OP-20260906T103000Z-restart-customer-web](../../../operations/2026/2026-09/OP-20260906T103000Z-restart-customer-web.md)
  — Approved rolling restart; health checks passed.
```

The resource timeline is an index, not a duplicate operation log.

## Incidents

Use `work/incidents/INC-<UTC timestamp>-<slug>.md` for a material anomaly.
Record detection, impact, affected resources, evidence, timeline,
communications, mitigations, current state, owner, and next update. Keep facts
separate from hypotheses. Incident creation and notification do not authorize
remediation.

## Scheduled inspections

Scheduled inspection is off by default. Before creating a timer or recurring
job, confirm:

- scope and exclusions
- IANA timezone and frequency or wall-clock time
- allowed checks, concurrency, and timeouts
- reporting and anomaly-notification behavior
- maintenance and silence windows
- report-only versus explicitly named remediation runbooks
- review date, end date, and pause/revocation conditions

Persist the confirmed policy in `work/inspections/policy.yaml`. Reconfirm any
change to scope, schedule, notification, or remediation authority. Give every
run its own tracked lifecycle and dated inspection record. The default for an
anomaly is to report and open an incident, not to repair it.
