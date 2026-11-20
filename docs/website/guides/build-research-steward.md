---
title: Build a traceable research and decision-brief workflow with research-steward
summary: Instantiate the research-steward template in the Web GUI, drive question framing, primary-source ledgers, citation verification, and structured decision briefs in conversation, and track unresolved follow-ups across sessions.
order: 86
---

# Build a traceable research and decision-brief workflow with research-steward

Use this guide to set up a long-lived `research-steward` agent that turns open evaluation questions into a bounded **question card**, a **source ledger** (`SRC-*`), a verified **claim/evidence matrix** (`CLM-*`), and a **decision brief** with explicit confidence levels and follow-up queues (`FU-*`).

- For tracking multiple open-source codebases and comparing commit histories over time on top of this template, see [Build an AI Agent ecosystem researcher for multi-repo research](./build-agent-ecosystem-researcher.md).
- To inspect the template contract and bundled skills, see the [Research Steward template](../templates/research-steward/).
- This workflow structures evidence and preserves contradictions for human judgment; it is not an automated fact-checking guarantee or permission to publish externally.

## Prerequisites

- A running Holon runtime with the [Web GUI](./use-web-gui.md) open (default `http://127.0.0.1:7878/`) and a [configured model](./configure-models.md).
- Web search/fetch access configured if your research relies on external documentation or announcements ([Use Web search and fetch](./use-web-tools.md)), or `gh` / local workspace repositories if evaluating codebases.
- No manual directory scaffolding or YAML setup is needed: the template preconfigures the agent contract (`AGENTS.md`) and research skills automatically.

## Steps

### 1. Create the agent from `research-steward` in the Web GUI

In the Web GUI, open **Agent Templates** (`/templates`), select **`research-steward`**, click **Create Agent**, and give it a descriptive unused ID such as `research-steward` or `tech-research`. (Or ask `main` in chat to create it from the `research-steward` template.)

The template automatically binds six skills in the new agent's isolated `agent_home`:

- `research-artifact`: structures outputs into `question-card`, `search-plan`, `source-ledger`, `claim-matrix`, `decision-brief`, `follow-up-queue`, and `provenance`.
- `citation-verifier`: audits claim-to-source entailment, scope, freshness, and counter-evidence before marking any claim `supported`.
- `ghx` & `sview`: inspect GitHub issues, releases, and repository files as primary sources without dumping full trees.
- `agentinbox`: tracks cross-session follow-up queues when a check depends on pending upstream releases or access.
- `writing-clearly-and-concisely`: polishes the brief only after the evidence pass is complete.

> **Optional (CLI):** On headless servers, run `holon agent create tech-research --template research-steward`. See [Create an agent from a template](./create-agent.md) for template installation details.

### 2. Frame the question card and boundaries in conversation

Open the new agent's conversation in the Web GUI (or from your mobile client when sharing a link on the go) and define the decision, audience, time window, and strict boundaries in one message:

> "Start a research package (`Q-001`) to compare whether our team should adopt Tool A or Tool B for self-hosted workflow orchestration.
> - Audience: engineering leads deciding Q1 architecture.
> - Time window: prioritize releases and docs from the last 6 months; record access dates.
> - Acceptance criteria: separate primary sources (official docs, source repos, release notes) from blog opinions, preserve contradictions, and mark anything unverified explicitly.
> - Boundaries: read and analyze only; do not modify project files, send emails, or publish externally."

If any material definition or scope constraint is missing, the `research-steward` contract instructs the agent to ask once and wait (`WaitFor` `operator_input`) rather than guessing assumptions. Because work persists in `WorkItem` state on the daemon, you can also send a repo or article URL from the iOS/Android client and review the finished brief when you return.

### 3. Enforce primary-source ledgers and claim verification

Before letting the agent write a prose summary, inspect its intermediate ledger or explicitly ask it to complete the verification pass first:

> "Build the `source-ledger` (`SRC-001`, ...) and run `citation-verifier` on every material claim (`CLM-001`, ...) before writing the decision brief. Treat search snippets only as leads, and include counter-evidence or version/scope caveats."

Under the `research-steward` contract:

- Every important source gets a stable ID (`SRC-001`), title/publisher, exact locator, publication or update date, access date, scope, and what it **cannot** establish.
- Every material claim gets a stable ID (`CLM-001`) and one explicit terminal status from `citation-verifier`: `supported`, `partially-supported`, `contradicted`, `unverified`, or `blocked`.
- Conflicting sources are preserved side by side with the reason they differ (time window, definition, methodology, or source quality), rather than smoothed into a single generic statement.

### 4. Review the decision brief and follow-up queue

Once the verification pass finishes, inspect the delivered `research-artifact` package:

```markdown
1. question-card     — decision, audience, scope, time window, acceptance test
2. search-plan       — sub-questions, source types, date bounds, deliberate exclusions
3. source-ledger     — SRC-001..N with locators, dates, and limitations
4. claim-matrix      — CLM-001..N linked to SRC-* and citation-verifier statuses
5. decision-brief    — answer first, supporting evidence, competing explanations, confidence
6. follow-up-queue   — FU-001..N unresolved checks, blockers, and closing conditions
7. provenance        — creator, verification timestamp, access limits
```

If you revisit the topic weeks later, ask the same agent to refresh specific `SRC-*` or `FU-*` entries instead of restarting from scratch. It keeps IDs stable and appends revision notes rather than silently rewriting history. You can also save standing audience or formatting preferences (such as preferred brief length or reply language) into `memory/operator.md` via conversation; see [Manage agent memory](./manage-agent-memory.md).

## Verification

Before acting on a decision brief, verify three things in the Web GUI output or generated Markdown artifact:

1. **Traceable locators:** Every `supported` claim in `claim-matrix` links to one or more `SRC-*` IDs with an exact section, commit, release note, or URL—not a search-engine snippet or homepage URL.
2. **Explicit uncertainty:** Inaccessible pages, stale dates, or contradictory sources appear in `unverified` / `blocked` rows and `follow-up-queue` (`FU-*`) rather than missing silently.
3. **Boundary compliance:** Confirm in the turn's tool execution details that no external writes, commits, or publications happened without your approval.

## Troubleshooting

- **Brief reads like generic search summaries:** Remind the agent to populate `source-ledger` and run `citation-verifier` before drafting `decision-brief`, and pass any local repo or primary documentation URL directly.
- **External source blocked or paywalled (`403` / login wall):** Keep the affected claim marked `unverified` or `blocked` in `claim-matrix` and supply an authorized local file or excerpt if needed; never ask the agent to guess missing numbers.
- **Scope keeps expanding across turns:** Narrow the `question-card` acceptance test in conversation or split a long-running multi-repo tracking effort into a dedicated setup following [Build an AI Agent ecosystem researcher](./build-agent-ecosystem-researcher.md).

## See also

- [Build an AI Agent ecosystem researcher for multi-repo research](./build-agent-ecosystem-researcher.md)
- [Use Web search and fetch](./use-web-tools.md)
- [Manage agent memory](./manage-agent-memory.md)
- [Create and customize a dedicated role agent with permission boundaries](./customize-agent.md)
- [Trust boundaries](/concepts/trust-boundaries.md)
