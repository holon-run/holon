# Agent state workload (D0)

This optimized, offline harness exercises loaded-runtime preview, accepted input
publication, and actual scheduler-to-provider request construction. It uses a
scripted provider; it never sends a network request. It is separate from the
small `context_snapshot` golden fixture and from real-LLM acceptance.

Build once from a frozen worktree, recording the compiler output and flags:

```sh
cargo bench --locked --bench agent_state_workload --no-run
```

Use the executable path printed by Cargo. The driver does not build or discover
an arbitrary cached executable. First run a pilot with the **complete fixture**:

```sh
python3 benchmarks/agent-state/run.py run \
  --binary /absolute/target/release/deps/agent_state_workload-HASH \
  --output /private/tmp/holon-agent-state-d0-pilot --label pilot \
  --warmup-per-agent 2 --measured-per-agent 10
```

Then run the frozen sampling policy at least three times per case:

```sh
python3 benchmarks/agent-state/run.py run \
  --binary /absolute/target/release/deps/agent_state_workload-HASH \
  --output /private/tmp/holon-agent-state-d0-baseline --label baseline --repeat 3
```

Each case runs in a fresh process. No fixture dimension can be reduced by an
environment variable. `normal` has 8 loaded Agents and 4 active request lanes;
`pressure` has 32 and 16. The two groups run sequentially while the other Agents
remain loaded and stopped. Each Agent starts with:

| Fixture | Exact quantity |
| --- | ---: |
| Historical messages | 100,000 |
| Linked historical turn/brief/transcript sets | 64 |
| Historical records in those four families plus the tool record | 100,193 |
| Accepted queued operator inputs | 100 |
| Body of each queued input | 16,384 bytes |
| Synthetic stdout in the latest linked tool record | 10,485,760 bytes |
| Prompt estimated-token budget | 8,192 |

History messages have short deterministic bodies. This is not 100,000 complete
turn bundles. Named creation already activates an empty runtime, so setup first
creates all identities and homes, shuts down and drains that Host, then seeds
cold storage. A fresh Host restores the seeded queue; no fixture write races a
loaded runtime. Setup records its own duration, actual I/O and memory.

History seeding is bench-only direct SQL inside the real `RuntimeDb::transaction`.
One history message is inserted through the typed repository and validates the
serialized payload, SHA-256, preview, kind, timestamp and fresh source revision
template. The remaining messages use prepared inserts with the same normalized
columns, monotonically increasing global source activity sequence, revision 1
and final per-Agent message sequence. Full row counts, normalized IDs/scope/turn
and work linkage, unique message sequences, source scope/activity/timestamps and
sequence allocator are checked; four messages also round-trip through the typed
repository and verify their payload hashes/previews. Linked turn/brief/transcript,
stdout, queued input and AgentState fixtures still use typed APIs. Direct SQL is
limited to fresh, stopped, cold synthetic history setup; runtime writes and
measured reads retain their production paths. This avoids per-message canonical
lookup and source `MAX(activity_seq)` scans without changing fixed dimensions.
Setup timings are only comparable when this seed mode matches.

The stdout has a small
`D0_STDOUT_PREVIEW` summary/preview and a full durable body. Preview and provider
request assertions reject materializing the full stdout into a request.

One untimed preview round warms each loaded Agent; five rounds are measured.
Preview includes acquisition and rendering through `RuntimeHandle::preview_prompt`.
It does not run the dispatch WorkItemScoped adoption or construct a provider
request. A separate phase accepts one new 16 KiB message per Agent and verifies
the ACK includes live queue publication (101 pending).

The dispatch phase uses the actual Host Start, scheduler, owner selector,
context/resource preparation, provider request builder and terminal writes.
The provider initially returns exactly one visible GetAgent call, restricted to
that same Agent, with no Text. A subsequent conversation containing its actual
successful tool result receives deterministic Text, ending the dispatch; a
possible tool-less delivery round receives Text as well. Initial versus follow-up
requests are identified from conversation/tool-result anchors. Every request
checks the current_input section contains this Agent’s pending operator marker. Each Agent's warmup barrier waits inside
the provider after its request is built. After all lanes arrive, reset the
probe and release them together. After the measured responses, hold the next
built request, snapshot metrics, then Stop outside the interval. Warm history
therefore grows across real scheduler/provider rounds.

Default sampling is `ceil(100 / loaded)` warmup requests and
`ceil(1000 / loaded)` measured responses **per Agent**: normal 13/125, pressure
4/32. One additional barrier response is inside the interval; its already-built
request is excluded. The final held request is counted as constructed but has
no response. Provider calls and context preparations are different quantities;
the artifact reports actual timing sample counts. At least two provider rounds occur per admitted dispatch (GetAgent then Text).
The pilot must verify this policy fits the 100 pending inputs; exhausted input or missing mandatory input
fails rather than silently changing the workload. Sampling overrides are for
the pre-freeze pilot; compared artifacts must use the same values.

The opt-in process-wide `IoProbe` starts before Host/RuntimeDb construction.
SQL counts are executed SQLite statements classified using `stmt_readonly`,
with transaction-control statements separate. They include business validation
and commits; they are not automatically context-only attribution. File counters
cover instrumented AGENTS, curated memory, plan and skill readers/discovery, as
declared by the probe implementation; they are neither syscalls nor all FS I/O.
Logical diagnostic stage counters are recorded separately and never renamed as
SQL/file counts. Actual timing hooks measure context preparation to initial
request construction, excluding network. `CommitPublication` starts at
`begin_mutation` entry, before reservation acquisition, and includes owner queue
wait, candidate preparation, SQL and publication; it is not SQL-ACK-to-publication
latency.

Memory reports instrumented System-allocator requested live/phase-peak bytes
and process lifetime peak RSS (`getrusage`). SQLite native allocations and
allocator overhead are excluded from the former; OS page cache is uncontrolled.
Resident-domain bytes and snapshot-pinned allocation bytes remain `null` until
the production state model provides exact accounting. Comparing the instrumented
System allocator to an uninstrumented production allocator is unsupported.

Compare same-machine, same-toolchain, same-instrumentation runs:

```sh
python3 benchmarks/agent-state/run.py compare \
  --baseline /private/tmp/holon-agent-state-d0-baseline \
  --candidate /private/tmp/holon-agent-state-d0-candidate \
  --output /private/tmp/holon-agent-state-d0-comparison.json
```

The normal gate uses the median of independent-run p95 values: context/request
and commit/publication no more than 1.10 times baseline, constructed-request
throughput at least 0.95 times baseline. At least three runs and 100 actual
timing samples per kind per run are required. Workload, sampling, request shape,
mandatory-root checks and recorded environment must match. Pressure, setup,
RSS and unavailable resident/pin metrics remain separately reported. Failure,
timeout or OOM stays in the run manifest; the driver never retries with smaller
data. Prompt topology/shape is not a full prompt-content golden: retain the
existing context golden and correctness suites for that contract.

Artifacts contain counters, sizes, IDs, topology hashes and deterministic fixture
metadata, not provider credentials, full requests or full stdout. Raw stdout,
stderr, binary SHA-256 and unsuccessful-run information are preserved next to
successful JSON artifacts. This harness does not establish zero ordinary reads,
backpressure/pin-release correctness, end-to-end UX or real-provider cost/cache
acceptance; those require the corresponding later implementation and tests.

The first full-fixture pilot (`pilot-normal-01` and `pilot-pressure-01`) failed
after seeding its first Agent: named creation had left the empty runtime loaded,
so activation returned that instance with zero pending inputs. No valid timing
samples were saved. Its manifest/stdout/stderr remain unchanged; use a new label
for the corrected cold-setup pilot. The old setup took approximately 124–146
seconds per Agent; bulk setup acceleration remains unmeasured until that pilot.
