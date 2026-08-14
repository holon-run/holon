# Scheduler E2E Test Suite

This directory documents the scheduler-specific E2E scenario matrix,
execution architecture, and CI layering plan. The actual case definitions
and runner functions live in the shared Docker E2E infrastructure:

- Case definitions: `tests/e2e/docker/manifest.json`
- Runner functions: `scripts/docker_e2e/runner.py`
- Drill harness: `scripts/docker_e2e/scheduler_drill.py`
- Entry point: `scripts/docker-e2e.py`

## Scenario Matrix

Cases tagged `e2e-tier-1` in `manifest.json` map to the Tier-1 core
scheduling scenarios. Existing cases cover additional Tier-1 scenarios
implicitly.

| Scenario ID | Case ID | Description | Key Assertions |
|---|---|---|---|
| SCHED-E2E-001 | `scheduler-task-wait-resume` | Autonomous task-result and external-wait continuity across an in-flight daemon crash | promoted command interruption; deterministic restart TaskResult; exact rejoin; brief binding; waits resolved |
| SCHED-E2E-002 | `scheduler-multi-workitem-scheduling` | Multi-WorkItem concurrent scheduling | both complete; no conflicts; 2 activations; 2 settlements; restart persistence |
| SCHED-E2E-003 | `scheduler-provider-failure-work-queue-retry` | Provider failure recovery | recovery turn scheduled; final brief; no death loop; idempotency key preserved |
| SCHED-E2E-005 | `scheduler-external-wait-resume` | WaitFor external trigger + resume | wait state correct; external callback wakes; WorkItem resumes; wait resolved |
| SCHED-E2E-006 | `scheduler-operator-wait-resume` | WaitFor operator_input + resume | wait state correct; operator message wakes; WorkItem resumes; wait resolved |

### Tier-2 (Nightly)

| Scenario ID | Case ID | Description | Key Assertions |
|---|---|---|---|
| SCHED-E2E-010 | `scheduler-concurrent-claim-fencing` | Interject during external wait creates new WorkItem | both complete; 2 settlements; activation chains; wait resolved; restart persistence |
| SCHED-E2E-011 | `scheduler-operator-interject-during-wait` | Operator interject during operator wait creates new WorkItem | both complete; 2 settlements; wait resolved; restart persistence |
| SCHED-E2E-012 | `scheduler-compaction-continuity` | WorkItem survives compaction and restart | compaction triggered; WorkItem completes; brief intact; restart persistence |
| SCHED-E2E-013 | `scheduler-worktree-isolation` | Agent creates and removes a worktree through model tools | worktree lifecycle; execution binding; clean git state; restart persistence |
| SCHED-E2E-014 | `scheduler-spawn-agent-supervision` | Agent spawns private_child and completes parent WorkItem | SpawnAgent returns agent_id + task_id; brief intact; restart persistence |
| SCHED-E2E-015 | `scheduler-checkpoint-replay` | Multiple WorkItems survive restart and converge | B completes; A waits; restart preserves states; both converge; exactly-once |

### Tier-3 (Release)

| Scenario ID | Description | Status |
|---|---|---|---|
| SCHED-E2E-020 | 10 concurrent WorkItem stress | planned |
| SCHED-E2E-021 | Random SIGKILL chaos (drill stress) | planned |
| SCHED-E2E-022 | Provider full-chain degradation | planned |
| SCHED-E2E-023 | External trigger duplicate/out-of-order | planned |

## Assertion Layers

Each case asserts three layers:

1. **State layer**: SQLite `runtime.sqlite` queries via `runtime_db_snapshot()`
   verifying WorkItem state, scheduling_state, settlement records, wait
   conditions, and activation chains. Release and nightly matrix runs execute
   every selected scheduler case once in an isolated `legacy` process and once
   in an isolated `canonical` process.
2. **Artifact layer**: Brief existence, content, and work_item binding via
   `brief()` and work-items API.
3. **Behavior layer**: Event sequence and tool execution checks via
   `events()` and `assert_tools()`.

## CI Layering

| Layer | Trigger | Scope | Timeout | Blocking |
|---|---|---|---|---|
| PR required | every scheduler/runtime Docker-relevant PR | all scheduler invariant cases through the deterministic provider; legacy + canonical | 45 min job / 120s per case | yes (`Scheduler E2E Required`) |
| PR live canary | `e2e-scheduler` label | real-provider external wait/resume smoke; legacy + canonical | 20 min job / 120s per case | no (`continue-on-error`) |
| Nightly | schedule | deterministic required matrix plus the independent live canary | 60 min | deterministic only |
| Release | release pipeline | core real-model suite, deterministic scheduler gate, and independent live canary | 90 min | core + deterministic |

The `scheduler-required` profile uses the dependency-free OpenAI Responses stub
in `tests/e2e/docker/openai_stub/`. It covers task-result, provider retry,
multi-WorkItem scheduling, wait/resume, claim/interject, compaction, worktree,
child supervision, and checkpoint/restart invariants in both scheduler engines.
Its exact tool and turn contract remains strict and its
`scheduler-coverage-report.json` is release-blocking.

The `scheduler-live-canary` profile uses a real provider and records the model
route, provider attempt/retry counts, tool counts, and behavioral variance in
`scheduler-live-canary-report.json`. Tool-order and forbidden-tool differences
are evidence, not invariant failures; runtime fatal errors, failed tools, and
failure to complete the smoke lifecycle still fail the canary. Canary failure
does not replace or override the deterministic gate result.

### Current Decisions

- **Required CI provider**: local deterministic OpenAI Responses stub, with no
  provider secret or PR label.
- **Live CI provider**: single low-cost model via env var, reported separately
  from deterministic runtime invariants.
- **PR blocking**: the deterministic profile is required; the live provider
  job remains optional and `continue-on-error`.
- **Case migration**: scheduler invariant cases remain in the shared
  `manifest.json`; each required case names a deterministic `stub_scenario`.
- **Drill harness**: `scheduler_drill.py` is reused for Tier-2 checkpoint
  replay; no separate Python module extracted yet.
