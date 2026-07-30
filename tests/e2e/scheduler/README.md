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
| SCHED-E2E-001 | `scheduler-autonomous-legacy` | Single agent autonomous loop | brief exists; settlement completed; no duplicate settlement; restart persistence |
| SCHED-E2E-002 | `scheduler-multi-workitem-scheduling` | Multi-WorkItem concurrent scheduling | both complete; no conflicts; 2 activations; 2 settlements; restart persistence |
| SCHED-E2E-003 | `scheduler-provider-failure-work-queue-retry` | Provider failure recovery | recovery turn scheduled; final brief; no death loop; idempotency key preserved |
| SCHED-E2E-004 | `scheduler-terminal-before-settlement-restart` | SIGKILL crash recovery | settlement idempotent; brief not lost; exactly-once |
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

1. **State layer**: SQLite `state.db` queries via `runtime_db_snapshot()`
   verifying WorkItem state, scheduling_state, settlement records, wait
   conditions, and activation chains.
2. **Artifact layer**: Brief existence, content, and work_item binding via
   `brief()` and work-items API.
3. **Behavior layer**: Event sequence and tool execution checks via
   `events()` and `assert_tools()`.

## CI Layering

| Layer | Trigger | Tiers | Timeout | Blocking |
|---|---|---|---|---|
| PR optional | `e2e-scheduler` label | Tier-1 subset | 20 min | no (`continue-on-error`) |
| Nightly | schedule | Tier-1 all + Tier-2 all | 60 min (job) / 15 min (suite `--timeout 900`) | creates issue on failure |
| Release | release pipeline | Tier-1 + Tier-2 + Tier-3 | 90 min | yes |

### Approved Decisions

- **CI Provider**: single low-cost model via env var; multi-provider matrix
  only in release pipeline (max 2 providers).
- **PR blocking**: `continue-on-error: true`; nightly is the regression gate.
- **Phase order**: Phase 1 (infrastructure) → 2 (Tier-1) → 3 (Tier-2) → 4
  (Tier-3); within Phase 2, crash recovery and provider failure are highest
  priority.
- **Case migration**: existing 4 scheduler cases remain in the shared
  `manifest.json`; new cases are added alongside them.
- **Drill harness**: `scheduler_drill.py` is reused for Tier-2 checkpoint
  replay; no separate Python module extracted yet.
