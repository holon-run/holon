---
title: "Manage agent memory: From long-term preferences and raw history retrieval to memory auditing and pruning"
summary: Have agents record durable preferences in natural dialogue, retrieve factual evidence from immutable raw history, and audit or prune obsolete memories when workflows change.
order: 150
---

# Manage agent memory: From long-term preferences and raw history retrieval to memory auditing and pruning

Long-lived agents must remember your personal coding habits, review requirements, and stable project facts across multiple turns, client disconnects, and weeks of continuous execution. However, typical agent systems either suffer from context amnesia as conversations grow or blindly compress all past history into lossy summaries—discarding critical causal chains, causing hallucinations, and conflicting with updated project rules.

In Holon, memory management is powered by a two-tier architecture: **curated explicit memory** (`memory/operator.md` and `memory/self.md`) alongside **strong raw history retrieval** (`MemorySearch` and `MemoryGet`). You can instruct your agent in natural language to persist habits, trace past decisions directly to verifiable raw evidence, and audit or prune outdated memories when technical decisions evolve.

## Prerequisites

- A running Holon runtime and [Web GUI](./use-web-gui.md) (default at `http://localhost:8080`) or CLI session.
- An active long-lived agent (such as `main` or a custom agent).
- No vector database scripts or database migrations required: all memory operations can be performed via conversational natural language or by inspecting Markdown files in the agent's home directory.

---

## Practical Steps

### 1. Understand memory layers and trust boundaries

Effective memory management starts with placing information at the right layer:

| Information Category | Storage Location | Ingestion & Lifecycle Design |
|---|---|---|
| **Operator preferences & style** | `memory/operator.md` | Auto-loads a compact slice (up to 1,500 characters) into prompt context on each turn. Captures language, style, and verification rules. |
| **Agent role facts & ledgers** | `memory/self.md` | Auto-loads a compact slice (up to 1,500 characters) on each turn. Captures the agent's standing domain, baselines, and durable system state. |
| **Immutable raw history & evidence** | Runtime event store & WorkItems (`MemorySearch` / `MemoryGet`) | Queried on demand; does not consume static prompt budget. Indexes past conversation turns, tool receipts, and WorkItem plans. |
| **Standing rules & authority boundaries** | `AGENTS.md` (agent & workspace) | Fully injected on every turn. Defines role boundaries, security gates, and operations requiring operator authorization. |
| **Active task execution & status** | Runtime WorkItem | Scheduling kernel. Tracks immediate objectives, plans, and checklist items. Memory never replaces active task state. |

> **Research Insight: Why explicit memory needs raw history retrieval as a baseline**
> Recent agent research (such as arXiv:2610.30755 and arXiv:2610.24671) highlights that aggressively abstracting conversation history into lossy summaries or graph memories frequently drops essential dependency chains and contextual facts ("losing evidence during memory creation"). Holon keeps explicit memories concise and high-signal, while empowering agents to trace exact evidence back to raw runtime event logs whenever complex reasoning or verification is required.

---

### 2. Save long-term preferences through natural dialogue

You do not need to manually browse filesystem paths or run database commands. Simply tell the agent your workflow preferences during normal conversation:

**Example operator prompt**:
> "Remember my standing collaboration preferences for future turns:  
> 1. Always deliver completion briefs in English with concise bullet points;  
> 2. Always run local static checks and unit tests before committing repository files;  
> 3. Include error context and parameters in error logs rather than bare strings."

**Agent response & file update**:
The agent recognizes this instruction as a stable operator preference and updates `agent_home/memory/operator.md`:

```markdown
# Operator Memory

- Always deliver completion briefs in English with concise bullet points.
- Run local static checks and unit tests before committing repository files.
- Include parameter context in error logs; avoid bare error strings.
```

These preferences are automatically loaded at the beginning of future turns across reboots and new client sessions.

---

### 3. Retrieve past decisions using raw evidence (avoiding lossy guesswork)

When revisiting past architectural decisions, incident postmortems, or code refactor rationales, instruct the agent to retrieve exact evidence from historical records rather than relying on fuzzy multi-turn recall.

**Example operator prompt**:
> "Last week when we refactored the scheduler, we discussed whether to adopt Architecture Option B (centralized event bus). Find the exact rationale and discussion evidence for why we decided against it."

**Agent evidence retrieval workflow**:
1. **Locate source records**: The agent calls `MemorySearch` with targeted keywords (e.g., `Option B event bus scheduler refactor`) across runtime evidence and past WorkItems.
2. **Bounded source inspection**: The agent feeds the resulting `source_ref` into `MemoryGet` to read the exact historical excerpt without filling prompt context with unrelated raw logs.
3. **Deliver an evidence-backed report**:

> "According to the discussion record in WorkItem `work_38a19` (2026-11-09), Option B was rejected for two specific reasons:  
> 1. The centralized event bus introduced cross-process lock contention, increasing wake latency by 35% under load;  
> 2. It compromised the trust boundary isolation between child agents.  
> Source references: `brief_84f910a` and `turn_129`."

Grounding the response in raw historical evidence eliminates hallucinations and ensures historical fidelity.

---

### 4. Audit and prune obsolete or conflicting memories

As projects evolve, older conventions and tools are superseded (e.g., migrating from a legacy shell script to GitHub Actions, or deprecating an external API). If left unmanaged, stale entries will cause conflicts and confusion.

Ask your agent to audit its memory files and remove outdated rules:

**Example operator prompt**:
> "Audit your memory rules regarding deployment. We have officially replaced the manual deploy script `deploy.sh` with automated GitHub Actions. Clean up the stale rule and update your deployment preferences."

**Agent auditing and pruning workflow**:
1. **Audit current explicit memory**: Reads `memory/operator.md` and `memory/self.md` for deployment-related entries.
2. **Detect conflicts**: Identifies the stale instruction `- Run ./scripts/deploy.sh staging before every release`.
3. **Prune and update**: Removes the obsolete command and records the new GitHub Actions workflow, reporting the exact diff to you:

> "Audited and updated memory rules:  
> - **Removed stale rule**: `Run ./scripts/deploy.sh staging before every release`  
> - **Added current rule**: `After merging code to main, await GitHub Actions deployment and verify live health checks`  
> Both files remain well within the 1,500-character context budget."

---

## Best Practices for High-Signal Memory

1. **Honor the 1,500-character budget**:
   `memory/operator.md` and `memory/self.md` are reserved for concise, long-lived preferences. Do not dump ephemeral task checklists, voluminous logs, or full project documentation here. Content beyond the budget can still be searched, but it will not be permanently auto-loaded.
2. **Separate preferences, rules, and task progress**:
   - Workflow preferences belong in `memory/operator.md`.
   - Security boundaries and tool permissions belong in `AGENTS.md`.
   - Current goals and intermediate milestones belong in a `WorkItem`.
3. **Memory never bypasses authority boundaries**:
   Memory provides auxiliary context, not elevated authority. Even if an agent's memory notes that "operator authorized automated deployments", high-risk operations (such as destructive cleanup or public releases) still require explicit runtime gates as defined in `AGENTS.md`.

---

## Troubleshooting

### Why didn't the agent follow a newly recorded preference?
- **Verify file persistence**: Ask the agent: "Show me the current contents of your loaded operator memory" to verify that the entry was actually written to `memory/operator.md`.
- **Check for conflicting rules**: If contradictory rules coexist, prompt the agent: "Remove old preference X and only adhere to new preference Y".
- **Check context budget truncation**: If the memory file exceeds 1,500 characters, trailing lines are truncated. Ask the agent to prune redundant lines.

### When should I use conversational prompts versus editing files directly?
- **Conversational prompts (recommended)**: Ideal for day-to-day tweaks. The agent ensures correct formatting and merges rules naturally.
- **Direct file edits**: Ideal for bulk imports or major restructuring. Open `agent_home/memory/operator.md` or `memory/self.md` in any text editor and format as clean Markdown.

---

## Related links

- [Memory System Concepts](/concepts/memory.md) — Architecture of Holon's layered memory model
- [Context Continuity & Recovery](/concepts/context-continuity.md) — Surviving reboots and multi-session resumption
- [Run Long-Lived Tasks](run-long-lived-task.md) — Managing persistent goals with WorkItems
- [Discover and Enable Agent Skills](use-skills.md) — Expanding agent capabilities with modular skills
