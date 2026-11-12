# Agent Runtime State Acceptance

The resident-state milestone uses one committed publication per loaded agent.
Current facts and accepted obligations stay resident, optional history follows
bounded domain selectors, and callers can explicitly read qualified cold
references. The production implementation tested here is
`f78eb6cb048a9cf8e10233c7191b9c660ba4334e` (2026-11-12).

## Implementation boundaries

- [AgentSnapshot](../../src/runtime/resident_state.rs) and
  [MutationOwner](../../src/runtime/mutation_owner.rs) define the publication
  and its authority. Preview, Admission, and Execution first try
  [Ready capture](../../src/runtime/context_capture.rs). Scoped preparation
  handles invalidation, missing coverage, or recovery.
- The [Input](../../src/runtime/resident_input_evidence.rs),
  [Work](../../src/runtime/resident_work.rs),
  [Task](../../src/runtime/resident_task.rs), and
  [Scheduling](../../src/runtime/resident_scheduling.rs) selectors bound
  optional history. Accepted input, required dependencies, and request pins
  have their own lifetimes.
- Cold access stays at named query boundaries. Examples include
  [MemoryGet source refs](../../src/tool/tools/memory_get.rs),
  [GetWorkItem by ID](../../src/tool/tools/get_work_item.rs), and
  [finite Event history by cursor](../../src/host/event_read.rs).
  The qualified read supplies the requested data without extending the
  ordinary context window.
- [Resource refresh](../../src/runtime/resource_refresh.rs) owns invalidation
  for known writes and explicit refresh. A refresh or bootstrap can pick up
  external edits.

## Verification

### Ready capture and performance

The optimized [D0 workload](../../benchmarks/agent-state/README.md) uses
100,000 historical messages and 100 pending inputs per agent. The complete
normal fixture uses 8 agents and 4 lanes with 13 warmup and 32 measured
responses per agent. The pressure fixture uses 32 agents and 16 lanes with 4
warmup and 8 measured responses per agent.

| Scenario | Preview Ready | Admission Ready | Execution Ready | Attributed capture SQLite statements | Context reservations |
| --- | ---: | ---: | ---: | ---: | ---: |
| Normal, three runs combined | 120/120 | 408/408 | 408/408 | 0 | 0 |
| Pressure, one run | 160/160 | 128/128 | 128/128 | 0 | 0 |

These are attempted captures across the sampled phases. Commit and other
business I/O have separate counters. The pressure run measured 444.90 ms
context/request p95 and 1,297.17 ms commit/publication p95. Its
instrumented process peak RSS was 1,113 MiB.

The normal comparison used three runs each of
`8674dfd251f25ca7a661e7a414f510c6b96202c9` and
`f78eb6cb048a9cf8e10233c7191b9c660ba4334e`. The D0 driver confirmed identical
fixture, request shape and recorded environment fields. Each run contributed
136 context/request samples and 4,312 commit/publication samples. The table
uses the median of the run-level results.

| Normal metric | Baseline | Candidate | Change | Gate |
| --- | ---: | ---: | ---: | --- |
| Context/request p95 | 364.85 ms | 278.10 ms | −23.8% | Pass |
| Commit/publication p95 | 308.52 ms | 253.65 ms | −17.8% | Pass |
| Constructed requests/s | 3.199 | 2.964 | −7.3% | Fail (−5% limit) |

The baseline throughput runs were 2.309, 3.199 and 3.219 requests/s. The
candidate runs were 3.267, 2.809 and 2.964 requests/s. The baseline runs and
the initial candidate run came from earlier sessions on the same host.
Concurrent compilation was observed during the repeat session. The throughput
gate remains open pending a controlled paired comparison. The full dispatch
probe counted about 9,500 more SQLite read statements per 132-request group
on the candidate. That counter covers business paths beyond Ready capture,
and it does not attribute the difference to an individual stage or establish
its latency cost. Pressure has a single candidate observation.

### Lifecycle and explicit cold reads

The focused library run passed 32 tests. The unload assertion also passed
four additional isolated runs.

| Boundary | Regression coverage |
| --- | --- |
| Loaded capture | Preview, Admission and Execution Ready tests deny storage and exercise capture while owner acquisition is blocked. [Context capture](../../src/runtime/context_capture.rs) |
| Unload and observer read | The actual Host unload drops its published-root ownership, preserves the public identity, and a following list read leaves the agent unloaded. Separate tests exercise owned-mutation drain, exact recovery before reopening admission, and unloaded State projection. [Host tests](../../src/host.rs) |
| Bounded selection and explicit cold reads | The rolling Input window shares unchanged records and avoids pinning the entire map. Finite Event history matches the original page without activation or a publication. MemoryGet accepts qualified command, task, and generic tool source refs. [Input window](../../src/runtime/resident_input_evidence.rs) · [Event history](../../src/host/event_read_tests.rs) · [MemoryGet](../../src/tool/tools/memory_get.rs) |
| Restart | Host restart preserves explicit Stop and projects pending input from the committed Queue with a legacy State row. [Host tests](../../src/host.rs) · [Queue/State regression](../../src/runtime/agent_summary_cut_tests.rs) |
| Indeterminate commit | Lost-ACK cases recover the original Brief event, current exact Queue and Control state, or qualified Episodes. Reconciliation preserves newer commits and prevents replay of the original effects. [Input](../../src/runtime/input_recovery_replay_tests.rs) · [Control](../../src/runtime/control_recovery.rs) · [Episodes](../../src/runtime/episode_recovery.rs) |

### Provider and Host

`cargo test --locked --test live_agent_state_acceptance` passed the fixed
scripted Host scenario and gateway-budget tests. The opt-in candidate arm
(`HOLON_STATE_ACCEPTANCE=1`, `HOLON_STATE_ACCEPTANCE_ARM=new`) passed all five
cases against `deepseek/deepseek-flash` in 10 provider wire sends. The cases
exercise operator-fact recall, observable cache reuse, tool continuation,
episode/Work scope and same-incarnation Host restart. The provider reported
4,736 cache-read input tokens on the second cache observation request.
The candidate arm uses a fresh isolated data directory. See the
[acceptance harness](../../tests/live_agent_state_acceptance.rs).

## Remaining rollout evidence

- [#3951](https://github.com/holon-run/holon/issues/3951) tracks daemon readiness
  on restart with an existing long-lived data set. The Host restart fixture
  validates continuity and recovery for its fixed history.
- The broader [#3478](https://github.com/holon-run/holon/issues/3478) rollout
  still needs a D0 comparison against the original pre-#3793 revision and a
  matched real Provider comparison for prompt cost and cache outcomes.
  [#3793](https://github.com/holon-run/holon/pull/3793) records the earlier
  migration experiments at their original revisions. Pressure RSS is
  observational. The root and window tests cover object ownership and release,
  with allocator and total-process behavior outside their scope.
- [#4069](https://github.com/holon-run/holon/issues/4069) tracks the persistence
  schema and the field-by-field review of `agent_states`.
