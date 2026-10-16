---
title: Manage agent memory
summary: Save durable preferences in the right scope and retrieve past work with traceable sources.
order: 150
---

# Manage agent memory

Save stable preferences so they remain available after a conversation grows or a
client disconnects. Retrieve past decisions from their sources before relying on
them. Memory is evidence and context; it does not replace current instructions
or the state of a WorkItem.

## Before you start

Choose the agent whose memory you intend to update. Use its actual AgentHome
location from runtime context; `agent_home` below is a descriptive name, not a
shell directory to copy literally. Keep project-specific rules in the project.

## 1. Choose where the information belongs

| Information | Destination |
|---|---|
| Stable operator preferences, such as response language | This agent's `memory/operator.md` |
| Durable facts about this agent's role or recurring work | This agent's `memory/self.md` |
| Standing role, responsibilities, and authority boundaries | This agent's `AGENTS.md` |
| Repository build commands and project conventions | Project guidance in the active workspace |
| Current objective, acceptance checks, and remaining work | Existing WorkItem plan and todo list |
| Longer notes or supporting material | A named file in the appropriate workspace, read directly when needed |

These curated files belong to an agent. Writing a preference in one agent's
memory does not automatically update every other agent.

## 2. Save a small, specific preference

Ask the target agent:

```text
Remember for future work with this agent: keep completion reports concise,
include verification results, and state any untested parts. Update
memory/operator.md without replacing existing unrelated preferences.
Tell me which file changed.
```

The agent should read the file before editing, merge the preference, and return
its confirmed location. Inspect that change. Do not store credentials, callback
URLs, copied transcripts, or temporary task logs as preferences.

Holon loads a bounded slice of `memory/operator.md` and `memory/self.md` into
context. Put the important durable entries near the start and remove stale
duplication. Longer content remains retrievable; it is not all present in every
turn.

## 3. Retrieve a past decision

Ask the same agent to find a specific decision and identify its evidence:

```text
Find the previous decision about the CI retry policy. Read the original source,
identify the WorkItem or message it came from, and tell me whether it still
matches the current configuration.
```

The agent first calls `MemorySearch`:

```json
{ "query": "CI retry policy decision", "limit": 5 }
```

It copies a returned `source_ref` unchanged into `MemoryGet`:

```json
{ "source_ref": "SOURCE_REF_FROM_SEARCH", "max_chars": 8000 }
```

`source_ref` is an opaque reference. Do not invent one from a file path, URL, or
WorkItem ID. If the result supplies a saved source artifact, read that artifact
in bounded ranges when the inline text is insufficient.

## 4. Check the evidence before acting

Confirm that the retrieved source refers to the correct project and decision.
Compare old conclusions with current files or WorkItem state. A historical
success report does not prove that today's tests pass.

For a preference, success means the entry exists in the intended file and the
agent can retrieve it. For a past decision, success means the answer identifies
the source and distinguishes the historical decision from the current state.

## If search finds nothing

- Inspect `index_status.stale_reasons`. The background indexer may still be
  catching up; a later retry is more useful than duplicating the memory entry.
- Try the original terminology or a WorkItem-specific phrase.
- Check scope: search defaults to the current workspace context. Broader
  workspace search is an explicit option, not permission to access other agents.
- Read ordinary project files and notes with file tools. `MemorySearch` indexes
  runtime evidence and the curated agent memory files, not every Markdown file.
- If a source is missing or incomplete, report that limit instead of inventing
  the earlier decision.

See [Memory](../concepts/memory.md) for the indexing model and
[Work items](../reference/work-items.md) for tracked progress.
