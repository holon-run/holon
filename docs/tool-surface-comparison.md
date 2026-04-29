# Holon vs Claude SDK Tool Surface Comparison

This document records the `PB4` comparison result after `PB2` added richer
benchmark metrics and after `SVS-401` reran a focused comparison with current
token and round counters.

The comparison focuses on two kinds of analysis tasks:

- open-ended project audit:
  - benchmark task: `holon-project-roadmap-audit`
  - clean comparison run: `pb2-metrics-roadmap-audit-v2`
- focused evidence and pipeline mapping:
  - benchmark tasks:
    - `analysis-evidence-improvements`
    - `read-granularity-holon-analysis-pipeline`
  - focused rerun: `svs401-compare-v1`

## Why This Comparison Exists

Earlier benchmark waves showed that Holon often used more tools on analysis
tasks than Claude Agent SDK. That was not enough to conclude that Holon had an
analysis bug.

There were at least three plausible explanations:

- Holon prompt/runtime strategy was over-reading
- Holon's tool surface was coarser, causing more exploration round-trips
- the benchmark harness was not measuring the right things

`PB2` added a first useful set of metrics:

- `read_ops`
- `search_ops`
- `list_ops`
- `unique_files_read`
- `unique_search_queries`
- `bytes_read`

## Current Comparison

### Open-Ended Audit Baseline

`pb2-metrics-roadmap-audit-v2`:

| Metric | Holon | Claude SDK |
|---|---:|---:|
| Success | yes | yes |
| Duration | 32.3s | 40.8s |
| Tool calls | 26 | 29 |
| Read ops | 19 | 19 |
| Search ops | 2 | 5 |
| List ops | 4 | 5 |
| Unique files read | 18 | 17 |
| Unique search queries | 2 | 10 |
| Bytes read | 204,757 | 75,010 |

Source artifacts:

- Holon summary:
  - `.benchmark-results/pb2-metrics-roadmap-audit-v2/summary.json`
- Holon tool trace:
  - `.benchmark-results/pb2-metrics-roadmap-audit-v2/holon-project-roadmap-audit/holon/run-01/tools.jsonl`
- Claude SDK transcript:
  - `.benchmark-results/pb2-metrics-roadmap-audit-v2/holon-project-roadmap-audit/claude_sdk/run-01/transcript.jsonl`

This older compare run is still useful for read granularity, but it predates
the current benchmark token and tool-latency instrumentation. It should not be
used to draw token-cost or per-tool latency conclusions.

### Focused Fresh Compare

`svs401-compare-v1`:

#### `analysis-evidence-improvements`

| Metric | Holon | Claude SDK |
|---|---:|---:|
| Success | yes | yes |
| Duration | 15.0s | 13.9s |
| Tool calls | 7 | 15 |
| Read ops | 5 | 5 |
| List ops | 1 | 10 |
| Unique files read | 5 | 5 |
| Bytes read | 1,927 | 1,927 |
| Input tokens | 10,496 | 8,034 |
| Output tokens | 956 | 1,284 |
| Model rounds | 8 | 1 |

#### `read-granularity-holon-analysis-pipeline`

| Metric | Holon | Claude SDK |
|---|---:|---:|
| Success | yes | yes |
| Duration | 11.8s | 22.8s |
| Tool calls | 7 | 14 |
| Read ops | 5 | 10 |
| List ops | 1 | 4 |
| Unique files read | 5 | 10 |
| Bytes read | 157,340 | 167,901 |
| Input tokens | 63,933 | 48,268 |
| Output tokens | 853 | 2,292 |
| Model rounds | 4 | 1 |

Fresh compare artifacts:

- `.benchmark-results/svs401-compare-v1/summary.json`
- `.benchmark-results/svs401-compare-v1/analysis-evidence-improvements/holon/run-01/tools.jsonl`
- `.benchmark-results/svs401-compare-v1/read-granularity-holon-analysis-pipeline/holon/run-01/tools.jsonl`
- `.benchmark-results/svs401-compare-v1/analysis-evidence-improvements/claude_sdk/run-01/transcript.jsonl`
- `.benchmark-results/svs401-compare-v1/read-granularity-holon-analysis-pipeline/claude_sdk/run-01/transcript.jsonl`

## What This Shows

### 1. The data does not support a simple "Holon over-reads" conclusion

The strongest signal from the open-ended audit baseline is that the two runners
performed almost the same number of file reads:

- Holon: `19`
- Claude SDK: `19`

They also touched almost the same number of unique files:

- Holon: `18`
- Claude SDK: `17`

The fresh focused compare strengthens that conclusion:

- on `analysis-evidence-improvements`, both runners read exactly `5` files
- on `read-granularity-holon-analysis-pipeline`, Holon read fewer files than
  Claude SDK (`5` vs `10`)

So the difference is not "Holon reads vastly more files".

### 2. The shape of exploration is different

Claude SDK relied more on discovery tools:

- more `search_ops`
- more `list_ops`
- far more `unique_search_queries`

Holon, by contrast, searched less and moved into file reads more directly.

On the focused fresh compare:

- `analysis-evidence-improvements`
  - Holon: `1` list + `5` reads
  - Claude SDK: `10` list + `5` reads
- `read-granularity-holon-analysis-pipeline`
  - Holon: `1` list + `5` reads
  - Claude SDK: `4` list + `10` reads

This suggests a tool-surface difference:

- Claude SDK can stay longer in `Glob/Grep/Read` discovery mode
- Holon tends to move from `ListFiles/SearchText` into more direct `ReadFile`
  pulls

### 3. Raw tool count is not a good primary metric

The total tool counts were close:

- Holon: `26`
- Claude SDK: `29`

If we had only looked at total tool count, we could have drawn the wrong
conclusion. The richer metrics show that the real difference is:

- similar number of file reads
- different search/discovery strategy
- much higher bytes read on Holon

The fresh compare adds one more nuance:

- Holon can finish focused analysis tasks with materially fewer tool calls
- Claude SDK often spends those extra calls on discovery/listing rather than on
  additional grounded reading

### 4. The current likely gap is read granularity and synthesis style, not basic analysis ability

The most plausible current interpretation is:

- Holon analysis ability is already competitive
- Holon's current analysis loop tends to read broader chunks once it commits to
  a file and then spend more model rounds synthesizing
- Claude SDK gets more mileage from its discovery-oriented tool surface and
  often answers in a single round

This makes `PB3` and future tool-surface work more concrete:

- Holon does not obviously need "more reading"
- Holon more likely needs:
  - better read targeting
  - better stopping heuristics
  - possibly narrower exploration helpers later
  - lower-overhead synthesis once enough evidence is already gathered

### 5. Token and latency data are now partially available, but not enough for a final cost verdict

`svs401-compare-v1` provides current token and model-round counters for the
focused tasks:

- Holon used more input tokens and more model rounds on both focused tasks
- Claude SDK used fewer rounds but often more tool calls
- Holon's recorded tool latency is currently near-zero because `ListFiles` and
  `ReadFile` are local operations in this harness, while Claude SDK transcript
  artifacts do not expose the same per-tool timing breakdown

So the project can now say:

- token and round cost are part of the tradeoff
- older compare runs should not be retrofitted into token conclusions
- per-tool latency is still asymmetric across the two runners

That means `SVS-402` should decide based on:

- discovery-step shape
- read granularity
- token/round cost where current data exists

not on raw tool count alone.

## Current Product Judgment

The correct product judgment is:

- do **not** label Holon's current analysis behavior as an outright
  over-reading bug
- do **not** optimize only for lower tool count
- do prioritize:
  - better analysis heuristics
  - better benchmark visibility
  - coordination tools that help analysis stay organized

In short:

Holon is already competitive on the analysis task, but it appears to get there
by reading larger chunks of evidence than Claude SDK. That is a refinement
target, not a blocking defect.
