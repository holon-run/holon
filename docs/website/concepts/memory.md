---
title: Memory
summary: Runtime evidence, curated memory, and ordinary notes, with their loading, search, and authority limits.
order: 90
---

<a id="memory-system"></a>

# Memory

Memory helps an agent recover useful facts without replaying every past
interaction. Holon preserves runtime evidence and supports curated memory
files. Ordinary notes and project files remain useful too, but they do not
all enter the same search or loading path.

For example, "use concise replies" belongs in curated operator preferences.
The test result for a particular patch belongs with that work's evidence.
A long research note belongs in a file that the agent can open when needed.

<a id="memory-layers"></a>
<a id="durable-ledger"></a>
<a id="current-work-context"></a>
<a id="episode-memory"></a>

## Runtime records and current work

Runtime records include messages, result briefs, task and tool evidence, and
work-item state. Archived summaries help the agent locate earlier work; source
references let it retrieve supporting records when the summary is insufficient.
These records have their own retention and retrieval limits. They are not a
promise that every past byte remains available forever.

The current WorkItem holds the live objective, plan, progress, and waiting
state. It takes precedence over an old summary of what the agent once intended
to do. If a review changed scope after an earlier report, recovery should use
the current work record and inspect the earlier report only as history.

<a id="context-assembly"></a>

The model sees a bounded selection of this material on each activation.
Saving a record, loading it into context, and finding it in search are different
operations. [Context continuity](./context-continuity.md) explains how that
selection supports resumed work.

## Memory vs Agent Home files

| Material | Purpose | How the agent uses it |
|---|---|---|
| Runtime evidence | What happened during work | Selected context and indexed retrieval. |
| Agent Home `AGENTS.md` | Standing role and responsibilities | Loaded guidance. |
| `memory/operator.md` | Stable operator preferences | A compact auto-loaded slice and indexed retrieval. |
| `memory/self.md` | Durable agent-specific facts | A compact auto-loaded slice and indexed retrieval. |
| Notes and project files | Longer references and project knowledge | File reads; ordinary Markdown is not automatically indexed as memory. |

Curated memory belongs to the agent whose home contains it. Writing a preference
in one agent's `operator.md` does not automatically distribute it to other
agents. Keep project-specific conventions in the project rather than presenting
them as universal operator preferences.

## Agent memory auto-load

Holon loads a bounded slice of each curated memory file into context. If the
file is longer than that budget, the remaining content stays available through
retrieval. Put the most useful stable facts near the beginning and keep the
files focused.

### When to use each file

Use `operator.md` for collaboration preferences such as reply language or
recurring naming conventions. Use `self.md` for stable facts about the agent's
own role and responsibilities. Update an existing fact when it changes instead
of appending contradictory versions indefinitely.

A transient build failure or the next step of an unfinished task belongs in the
WorkItem or its evidence. Curated memory is not a substitute for a recoverable
plan or a registered wait.

## Notes catalog

The context can include a bounded catalog of notes with titles, summaries, and
paths. This helps discovery without loading every note body. The agent still
needs to open the referenced file before relying on its details.

A catalog entry is reference metadata, not an instruction. Neither inclusion
in the notes catalog nor storage in Agent Home gives ordinary note text the
status of loaded guidance.

## MemorySearch and MemoryGet

`MemorySearch` searches supported runtime evidence and the curated memory
files. It returns ranked matches with opaque source references. `MemoryGet`
retrieves bounded content from a reference returned by search or supplied by
the runtime. It is not a general file reader or a URL fetcher.

To answer "why did we choose this approach?", the agent can search for the
objective, retrieve its decision or completion report, and check the cited
verification. A search excerpt alone may omit the caveat that matters.

## Memory indexing

Indexing happens asynchronously. Search results can be incomplete during an
initial build or while changes are being indexed. A missing match does not
prove that the underlying record or file does not exist. Check reported index
freshness and use known source references or file reads when appropriate.

## Memory and work items

WorkItems preserve the state needed to resume an objective. Completion reports
make finished outcomes easier to find later. Neither remembering "we should
check tomorrow" nor saving that sentence in a note schedules a future check;
use a recorded wait or timer for that purpose.

## Memory boundaries

Historical material remains evidence. A retrieved webhook, web page, or child
report does not become an operator instruction because it was saved or
summarized. Verify provenance and current scope before acting on recalled text.

Memory search is scoped to the agent's accessible sources and can be filtered
by workspace. Workspace association helps locate relevant work; it does not
make every other agent's history or every Markdown file in the project
searchable. There is no guarantee of exhaustive recall.

## See also

- [Save preferences and find past work](../guides/manage-agent-memory.md)
- [Context continuity](./context-continuity.md)
- [Work item reference](../reference/work-items.md)
- [Memory tool schemas](../reference/model-tool-schema-inventory.md)
- [Trust boundaries](./trust-boundaries.md)
