# Runtime Database Storage Migration Implementation Plan

Related handle:

- `rfc-runtime-ledger-files-and-relations`

Related RFC:

- [Runtime Ledger Files and Relations](../rfcs/runtime-ledger-files-and-relations.md)

## Current repository posture

Holon currently keeps runtime state and evidence primarily under
agent-local `.holon/ledger/` JSONL files and derives current state by replaying
or reducing records per domain. The database direction in
`rfc-runtime-ledger-files-and-relations` changes that target:

```text
runtime.db  = canonical current runtime state + indexed evidence + audit stream
agent_home  = agent-local files, memory, skills, notes, plans, and artifacts
JSONL       = compatibility, audit mirror, or legacy import source
```

The migration should not become a generic "ledger table" rewrite. Runtime-owned
lifecycle state should move into explicit current-state tables, while evidence
and large artifacts remain referenced by id, path, hash, and bounded preview.

## Target invariants

The implementation PRs should preserve these invariants:

1. One Holon runtime installation owns one shared runtime database.
2. Agents are logical partitions in that database, usually through `agent_id`.
3. Current lifecycle state is canonical in domain state tables after cutover.
4. JSONL files are never silently mixed back into a domain once that domain is
   marked `canonical_source = db`.
5. Every state mutation goes through a domain repository or service, not ad hoc
   SQL scattered across tool handlers.
6. A state mutation updates current state, evidence references, causation or
   correlation metadata, and audit events in one transaction when those records
   are part of the same domain transition.
7. Large content may stay in files or artifacts. The database stores refs,
   hashes, bounded previews, and metadata.
8. Import is per domain, idempotent, resumable, and protected by a runtime
   exclusive lock.

## Shared migration metadata

The first database PR should establish the metadata needed by every later PR:

```sql
schema_migrations(
  version integer primary key,
  name text not null,
  applied_at text not null
);

storage_domains(
  domain text primary key,
  schema_version integer not null,
  import_status text not null,
  canonical_source text not null,
  source_checkpoint_json text,
  imported_at text,
  updated_at text not null
);
```

`schema_migrations` answers "is the database schema at the expected version?"
`storage_domains` answers "has this domain imported its legacy state and cut
over to DB?"

Code should never use table existence as the cutover test.

## Startup and cutover algorithm

Runtime startup should converge on this shape:

```text
open runtime database
acquire runtime exclusive lock
run pending schema migrations
for each required storage domain:
  read storage_domains row
  if domain is missing, pending, importing, or failed:
    mark domain importing
    import legacy JSONL snapshot for that domain
    validate imported current state
    mark domain complete and canonical_source=db
return RuntimeStore::Db
```

The import step should run inside one transaction per domain when practical:

```text
BEGIN
  mark domain importing
  rebuild or upsert imported state rows
  validate counts, latest revisions, active/current invariants
  mark domain complete
COMMIT
```

If a domain import fails, it must not mark `canonical_source = db`. A later
startup should be able to retry without duplicating state rows or moving a row
backward to an older revision.

## Current cutover posture

After the first current-state domain migrations, JSONL files are still present,
but their role is explicit per domain:

| Domain | DB posture | JSONL posture |
| --- | --- | --- |
| `work_items` | `canonical_source = db`; runtime reads current state from the `work_items` table. | Legacy compatibility/export mirror only. `work_items.jsonl` must not be replayed as current state after DB import completes. |
| `tasks` | `canonical_source = db`; runtime task queries use the `tasks` table. | Legacy compatibility/export mirror only. `tasks.jsonl` remains a historical stream, not a recovery source after cutover. |
| `external_triggers` | `canonical_source = db`; active trigger lookup and token routing use `external_triggers`. | Legacy compatibility/export mirror only. |
| `evidence` | `canonical_source = jsonl+db-index`; DB tables index bounded evidence previews and query keys. | `messages.jsonl`, `transcript.jsonl`, `tools.jsonl`, `briefs.jsonl`, and `delivery_summaries.jsonl` remain the import/source streams for large evidence content. |
| `audit_events` | `canonical_source = jsonl+db-index`; `audit_events` is the query/index sink. | `events.jsonl` remains the live audit mirror and cursor compatibility stream. |

Startup validates this posture through `storage_domains` instead of table
existence. A missing row, failed import, non-`complete` import status, or
unexpected `canonical_source` is reported as a cutover diagnostic. Import
failures leave a `failed` domain row with an error checkpoint so the next
startup has a clear retry path rather than silently rolling back to an unknown
state.

## PR sequence

### PR 1: Runtime database foundation

Purpose: introduce the shared database infrastructure without moving a
production domain yet.

Scope:

- Add a `RuntimeDb` or equivalent runtime database module.
- Add database path resolution for one runtime database, for example:

  ```text
  ~/.holon/state/runtime.sqlite
  ```

- Add migration runner support.
- Add runtime database pragmas such as WAL and `foreign_keys = ON`.
- Add runtime exclusive lock handling for startup, migration, and import.
- Add `schema_migrations`, `storage_domains`, `agents`, and minimal
  `audit_events` tables.
- Add a transaction helper that domain repositories can use later.
- Add test support for temporary runtime databases.

Out of scope:

- Do not migrate work items, tasks, queues, waits, messages, or transcripts yet.
- Do not remove JSONL writes.
- Do not introduce a generic append-only ledger table as the main state model.

Acceptance:

- A fresh runtime can create the database and run migrations.
- Re-running startup is idempotent.
- Schema version is determined from `schema_migrations`, not table existence.
- Tests can create isolated temporary DBs without touching a real agent home.

Useful verification:

```bash
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --all-targets
cargo test runtime_db --quiet
```

### PR 2: Work item state table, repository, and import

Purpose: migrate the first real current-state domain while keeping scheduler
risk low.

Scope:

- Add a `work_items` state table:

  ```sql
  work_items(
    work_item_id text primary key,
    agent_id text not null,
    state text not null,
    objective text not null,
    plan_status text,
    readiness text,
    revision integer not null,
    current_focus integer not null default 0,
    created_at text not null,
    updated_at text not null,
    completed_at text,
    plan_artifact_path text,
    last_turn_id text,
    last_message_id text,
    causation_id text,
    correlation_id text,
    payload_json text
  );
  ```

- Add indexes for `agent_id`, `state`, `readiness`, and current focus.
- Add `WorkItemRepository` or `WorkItemService`.
- Move `CreateWorkItem`, `GetWorkItem`, `ListWorkItems`, `UpdateWorkItem`,
  `PickWorkItem`, and `CompleteWorkItem` to the repository.
- Keep plan bodies in `agent_home/work-items/<id>/plan.md`; store only path and
  metadata in DB.
- Add a per-domain importer from existing work item JSONL records:
  - reduce create/update/complete records to the latest current state;
  - preserve latest `revision`;
  - restore current focus;
  - keep historical JSONL as legacy evidence, not current state.
- Mark `storage_domains.work_items` complete only after validation.

Out of scope:

- Do not migrate task, wait, queue, transcript, or message records in this PR.
- Do not delete legacy JSONL fallback yet.

Acceptance:

- A work item created through the tool surface can be queried from DB without
  scanning JSONL.
- Listing work items is an indexed DB query partitioned by `agent_id`.
- Revision checks prevent state rollback.
- Importing the same legacy work item data twice does not duplicate rows or move
  revisions backward.
- Once `canonical_source = db`, work item reads do not silently fallback to JSONL.

Useful verification:

```bash
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --all-targets
cargo test work_item --quiet
cargo test work_item_import --quiet
```

### PR 3: Task state table, repository, and import

Purpose: move task lifecycle state into the shared database so parent/child
coordination and command-task metadata have one canonical source.

Scope:

- Add a `tasks` table:

  ```sql
  tasks(
    task_id text primary key,
    owner_agent_id text not null,
    parent_agent_id text,
    child_agent_id text,
    kind text not null,
    status text not null,
    summary text,
    input_target text,
    wait_policy text,
    output_path text,
    result_summary text,
    exit_status integer,
    terminal_reentry integer not null default 0,
    revision integer not null,
    created_at text not null,
    updated_at text not null,
    completed_at text,
    last_turn_id text,
    last_message_id text,
    causation_id text,
    correlation_id text,
    payload_json text
  );
  ```

- Add indexes for owner agent, parent agent, child agent, status, and active
  task queries.
- Add `TaskRepository` or `TaskService`.
- Move `TaskList`, `TaskStatus`, `TaskInput`, `TaskStop`, task terminal result,
  and child-agent supervision metadata to DB-backed lifecycle reads/writes.
- Keep large stdout, stderr, artifacts, and model-visible previews in files or
  artifact storage; DB rows store refs and summaries.
- Add importer for legacy task records:
  - reduce task transition records to latest lifecycle state;
  - preserve terminal metadata;
  - preserve active task rows only when still meaningful;
  - attach output paths and artifact refs without copying large content.

Out of scope:

- Do not move queue claiming or wait-condition satisfaction in this PR unless
  required by a task transition boundary.
- Do not replay command execution or child-agent work from old records.

Acceptance:

- Parent and child task handles can be inspected from the shared DB.
- Completed, failed, cancelled, and running task states remain distinguishable.
- `TaskOutput` can still resolve output refs while lifecycle metadata comes from
  DB.
- Import does not create runnable work from historical terminal task records.

Useful verification:

```bash
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --all-targets
cargo test task --quiet
cargo test task_recovery --quiet
```

### PR 4: Scheduler control-plane state and import

Purpose: move runnable/blocked routing state into DB indexes so scheduler
decisions no longer scan agent-local JSONL.

Scope:

- Add `wait_conditions`, `queue_entries`, `timers`, and `external_triggers`
  current-state tables.
- Add repositories or services for:
  - `WaitFor`;
  - scheduler queue enqueue/dequeue/claim;
  - timer creation, recheck, and satisfaction;
  - external trigger registration, wake routing, delivery accounting, and
    revocation.
- Move scheduler projection queries to DB indexes.
- Ensure `WaitFor` attaching to a current work item remains a transactional
  state transition when it also changes work-item readiness.
- Add importers for active control-plane records:
  - import active waits only;
  - import pending queue entries only;
  - import live timers only;
  - import active external triggers only;
  - keep expired, satisfied, cancelled, and historical records as evidence or
    audit imports.

Candidate table shapes:

```sql
wait_conditions(
  wait_id text primary key,
  agent_id text not null,
  work_item_id text,
  task_id text,
  wake_kind text not null,
  resource text,
  status text not null,
  reason text,
  recheck_after_ms integer,
  created_at text not null,
  updated_at text not null,
  satisfied_at text,
  revision integer not null,
  payload_json text
);

queue_entries(
  queue_id text primary key,
  agent_id text not null,
  message_id text not null,
  priority text not null,
  status text not null,
  available_at text,
  claimed_at text,
  created_at text not null,
  updated_at text not null,
  payload_json text
);

timers(
  timer_id text primary key,
  agent_id text not null,
  work_item_id text,
  resource text,
  status text not null,
  fire_at text not null,
  created_at text not null,
  updated_at text not null,
  fired_at text,
  revision integer not null,
  payload_json text
);

external_triggers(
  external_trigger_id text primary key,
  target_agent_id text not null,
  waiting_intent_id text,
  trigger_url text,
  token_hash text not null,
  status text not null,
  created_at text not null,
  revoked_at text,
  last_delivered_at text,
  delivery_count integer not null,
  payload_json text
);
```

The first DB version intentionally keeps `external_triggers` as a standalone
state table because it maps directly to the current `external_triggers.jsonl`
domain and avoids mixing capability lifecycle state into `agents` too early.
The current product contract is one active default wake-hint trigger per agent,
so `scope` and `delivery_mode` are not first-class DB dimensions. Legacy import
normalizes old records to agent-scoped `wake_hint` records and preserves the
full runtime record in `payload_json` for compatibility.
Default agent ingress can be folded into an agent property or split into a
dedicated secret/capability service later, after DB-backed state is stable.
Until then, `trigger_url` and `token_hash` remain capability-bearing fields:
they may be stored for compatibility with the current runtime model, but they
must not be projected into ordinary prompt context, debug summaries, or generic
agent state dumps.

Out of scope:

- Do not index every message/transcript/tool execution yet.
- Do not refactor default agent ingress into `agents`.
- Do not introduce a separate capability secret store.
- Do not change external capability semantics beyond moving current
  `ExternalTriggerRecord` state and routing to DB.

Acceptance:

- Scheduler runnable and blocked queries are DB-backed.
- External wake routing can find the owning agent and work item from DB.
- Task-result wake routing can satisfy a wait and enqueue the target agent in a
  single transaction.
- Import does not resurrect expired waits, old queue entries, or revoked
  triggers.

Useful verification:

```bash
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --all-targets
cargo test scheduler --quiet
cargo test wake_hints --quiet
cargo test external_trigger --quiet
```

### PR 5: Evidence indexing and audit event sink

Purpose: make runtime evidence queryable without treating evidence as state
recovery input.

Scope:

- Add evidence tables for bounded, queryable metadata:
  - `messages`;
  - `transcript_entries`;
  - `tool_executions`;
  - `model_requests`;
  - `model_responses`;
  - `briefs`;
  - `delivery_summaries`;
  - `artifact_metadata`.
- Store large payloads through `content_ref`, `content_hash`, `preview`, and
  `payload_json`.
- Add `EvidenceRepository` and `AuditEventSink`.
- Ensure state-domain repositories can attach evidence ids and emit audit
  events transactionally when they own the same transition.
- Add importers that preserve legacy JSONL records as immutable evidence rows
  rather than current state.

Out of scope:

- Do not make evidence replay rebuild current state.
- Do not load full transcript/model/tool payloads into database rows by default.

Acceptance:

- Evidence can be queried by `agent_id`, `turn_id`, `message_id`, `task_id`, and
  `work_item_id` where those relationships exist.
- Audit events remain observer-facing and cursorable.
- Current state recovery does not depend on replaying evidence rows.

Useful verification:

```bash
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --all-targets
cargo test evidence --quiet
cargo test audit --quiet
```

### PR 6: Domain cutover hardening and JSONL retirement

Purpose: remove ambiguous source-of-truth behavior after domain imports are
stable.

Scope:

- Add explicit feature/config posture for legacy JSONL export or audit mirror.
- Remove JSONL fallback reads for domains whose `canonical_source = db`.
- Keep compatibility export paths clearly labeled as non-canonical.
- Add startup diagnostics for:
  - missing migration;
  - failed domain import;
  - mixed canonical sources;
  - stale or conflicting legacy writes.
- Add recovery commands or diagnostics for retrying failed domain imports.
- Document which legacy JSONL files remain as audit/export and which are no
  longer written.

Out of scope:

- Do not delete historical user data.
- Do not require all evidence history to be imported before state domains are
  canonical in DB.

Acceptance:

- A runtime cannot accidentally use both JSONL and DB as canonical state for the
  same domain.
- Import failure leaves a clear failed state and retry path.
- New runtime installations start directly with DB canonical domains.
- Existing runtime installations can import legacy JSONL once and continue on
  DB.

Useful verification:

```bash
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --all-targets
cargo test migration --quiet
cargo test recovery --quiet
```

## Post-DB follow-up: recent turns context spine

After the DB-backed state domains, evidence indexing, and JSONL cutover
hardening above are stable, the recent-context renderer can move to the
turn-centered projection described in
[`recent-turns-context-spine`](recent-turns-context-spine.md).

Keep this out of the first database migration wave. The improvement depends on
queryable turn, message, brief, delivery, and tool-execution evidence; doing it
before the DB refactor would force the renderer to keep reconstructing turns
from parallel JSONL projections and would obscure the source-of-truth migration.

## Data migration details by domain

### Work items

Import latest state by `work_item_id` and `revision`.

Current-state rows should represent only the latest work item state. Historical
updates can be linked as evidence or audit records.

Validation:

- one row per work item id;
- no revision rollback;
- at most one current-focus work item per agent unless the runtime contract
  explicitly allows multiple focus slots;
- completed work items do not remain runnable.

### Tasks

Import latest state by `task_id` and lifecycle revision.

Terminal task records should stay terminal. Active task records should only be
imported as active if the runtime can still supervise or reconcile them.

Validation:

- terminal task states are not requeued;
- output refs point to existing or explicitly missing artifacts;
- child-agent task handles preserve parent/child ownership fields.

### Waits, timers, queue entries, and external triggers

Import only currently meaningful control-plane state.

Validation:

- active waits point to existing agent/work item/task rows when applicable;
- pending queue entries have message refs;
- timers have future or still-actionable deadlines;
- external triggers are active, route to an agent, and preserve current
  `ExternalTriggerRecord` fields needed to render or validate existing
  callbacks;
- satisfied, cancelled, expired, processed, or revoked records are evidence,
  not active state.

### Messages, transcripts, tool executions, and model artifacts

Import as evidence indexes, not current state.

Validation:

- ids remain stable;
- provenance fields such as origin, trust, priority, turn id, and agent id are
  preserved;
- large content uses refs, hashes, and previews instead of unbounded DB payloads.

## Rollout rules

Each domain should follow the same lifecycle:

```text
1. create schema and repository
2. add DB write path
3. add DB read path for canonical domains
4. add per-domain importer
5. validate imported state
6. mark canonical_source=db
7. stop JSONL canonical fallback
8. keep or remove JSONL only as explicit compat/audit/export
```

For Holon's current stage, prefer an exclusive startup lock over an online
dual-writer migration. If online migration later becomes necessary, it should be
a separate RFC or implementation note because it changes the cutover contract.

## Verification matrix

Every PR in the sequence should run:

```bash
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --all-targets
```

Domain PRs should also add focused tests for:

- fresh DB initialization;
- migration re-run idempotency;
- interrupted or failed import retry;
- legacy JSONL import into current-state rows;
- no fallback to JSONL after `canonical_source = db`;
- agent partitioning by `agent_id`;
- revision and status transition validation;
- audit/evidence references for state mutations.

## Non-goals for the first migration wave

- No distributed database model.
- No per-agent private DB as the default.
- No strict event-sourced `domain_events` layer.
- No deterministic replay of model calls, tool side effects, web fetches, or
  command executions.
- No big-bang deletion of JSONL history.
- No UI-first schema design.
