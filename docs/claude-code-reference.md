# Claude Code Reference For Holon

This document captures the parts of Claude Code that are most relevant to
`Holon`.

The goal is not to clone Claude Code. The goal is to preserve the runtime ideas
that are useful when building a headless, event-driven, long-lived agent
system.

When future agents work on `Holon`, they should use this document as the first
reference before re-reading the larger research notes.

## Reference Repositories

Primary reading base:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code`

Supplemental fill-in reference:

- `/Users/jolestar/opensource/src/github.com/claude-code-best/claude-code`

Working rule:

- Prefer `sanbuphy/claude-code-source-code` when reasoning about the public
  runtime shape.
- Use `claude-code-best/claude-code` only when a feature-gated or missing module
  needs a supplementary read.

## What Holon Should Learn From Claude Code

Claude Code is most interesting to Holon in these areas:

- the single-session runtime spine
- queue-based message intake
- explicit separation of user-facing output from internal trace output
- proactive wake / sleep behavior
- background task and subagent orchestration
- channel and remote-control style external ingress
- trust boundaries across operator input, external messages, and system events

Holon should not inherit:

- TUI-first architecture
- enterprise product gating
- model-vendor-specific assumptions
- broad product surface unrelated to the core runtime

## 1. Runtime Spine

The main runtime spine is not the TUI. It is the composition of:

- entrypoint routing
- session-level engine
- per-turn query loop
- tool orchestration
- task orchestration

Relevant files:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/entrypoints/cli.tsx`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/main.tsx`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/QueryEngine.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/query.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/Tool.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/utils/toolExecution.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/utils/toolOrchestration.ts`

Design lesson for Holon:

- Keep a clear split between:
  - process bootstrap
  - session runtime
  - single-run loop
  - tool execution layer
- Do not let UI concerns become the composition root.

## 2. Queue-Centered Runtime

Claude Code repeatedly normalizes very different triggers into one prompt queue.
This is the most important runtime pattern for Holon.

Triggers that end up as queued work include:

- local user input
- bridge or remote-control inbound messages
- MCP channel notifications
- scheduled tasks
- proactive ticks
- background task completions

Relevant files:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/cli/print.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/utils/messageQueueManager.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/types/textInputTypes.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/utils/messages.ts`

Design lesson for Holon:

- Preserve one normalized queue model across operator, timer, channel, webhook,
  task, and system-originated work.
- Preserve provenance in the queue item itself.
- Avoid special “side doors” that bypass the main session loop.

## 3. Session-Level Engine Versus Turn-Level Loop

Claude Code cleanly separates:

- session lifecycle management
- one turn of agent execution

Relevant files:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/QueryEngine.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/query.ts`

Design lesson for Holon:

- Keep session state separate from one run or one prompt execution.
- A session should own queue, tasks, sleep state, and wake reason.
- A run should own prompt assembly, model call, tool turns, and completion.

## 4. Subagents And Background Tasks

Claude Code’s subagents are not a separate runtime. They reuse the same query
core behind a task wrapper.

Relevant files:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/tools/AgentTool/AgentTool.tsx`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/utils/createSubagentContext.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/tasks/`

Important observations:

- `AgentTool` behaves like a scheduler frontend, not just a single tool call.
- Subagents can run locally, in the background, remotely, or in an isolated
  worktree.
- Worktree is an isolation strategy, not a separate agent architecture.

Design lesson for Holon:

- Define tasks first, then decide how they execute.
- Rejoin background work through the same queue contract used by other events.
- Never let spawned work lose parent identity.

## 5. Proactive Runtime, Tick, And Sleep

Claude Code contains a real proactive runtime direction.

Relevant files:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/main.tsx`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/constants/prompts.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/cli/print.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/tools/SleepTool/prompt.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/tools/ScheduleCronTool/prompt.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/hooks/useScheduledTasks.ts`

Important observations:

- proactive mode is not just a prompt tweak
- the runtime injects `<tick>`-style wakeups
- the model is expected to choose `Sleep` when no useful work remains
- scheduled tasks become ordinary wake triggers

Design lesson for Holon:

- `sleep` should be a first-class runtime state
- `tick` should be explicit, not implicit
- timers and system-generated follow-ups should use the same lifecycle model as
  external events

## 6. External Ingress: Remote Control And Channels

Claude Code supports more than local terminal input.

### Remote Control

Remote control is the formal path for sending new user-style messages into a
running session.

Relevant files:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/bridge/initReplBridge.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/bridge/bridgeMessaging.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/hooks/useReplBridge.tsx`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/cli/print.ts`

Important observation:

- inbound remote-control user messages are enqueued as real prompt work and then
  trigger `run()`

### Channels

Claude Code also supports external channel-style input over MCP notifications.

Relevant files:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/services/mcp/useManageMCPConnections.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/services/mcp/channelNotification.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/services/mcp/channelPermissions.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/utils/messages.ts`

Important observations:

- channel notifications are queued with explicit metadata
- channel input is marked as external and untrusted
- channel input does not directly execute slash commands
- channel input can still enter model context and indirectly trigger tool use

Design lesson for Holon:

- support external ingress, but keep operator input and external channel input
  separate in both data model and policy
- avoid transport-specific bypass paths

## 7. Trust Boundary Model

One of the strongest Claude Code patterns is that it distinguishes the “main
user” from external inputs.

Relevant files:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/types/textInputTypes.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/utils/messages.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/services/mcp/useManageMCPConnections.ts`

Important observations:

- keyboard or bridge-origin input is treated as the primary user surface
- channel input is explicitly marked as “not your user”
- provenance survives into later runtime logic such as title extraction and
  queue handling

Design lesson for Holon:

- `origin` must survive normalization
- `trust` must not be inferred only from message content
- external events should be able to influence planning without silently
  inheriting operator authority

## 8. Brief Output And User-Facing Delivery

Claude Code’s `BriefTool` is useful because it separates user-facing delivery
from internal assistant chatter.

Relevant files:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/tools/BriefTool/BriefTool.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/tools/BriefTool/prompt.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/components/Messages.tsx`

Important observations:

- normal model text is not treated as the sole user-facing answer path
- acknowledgements and results are intentionally structured
- attachments are part of the delivery concept

Design lesson for Holon:

- keep user-facing output explicit
- do not make operators scrape internal traces to understand what happened

## 9. Context Compaction

Claude Code’s context compaction is a pipeline, not a single summary step.

Relevant files:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/query.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/services/compact/microCompact.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/services/compact/prompt.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/services/compact/sessionMemoryCompact.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/utils/toolResultStorage.ts`

Known pipeline pieces:

- compact boundary selection
- tool result budget trimming
- `snip`
- `microcompact`
- collapse
- autocompact

Design lesson for Holon:

- do not treat “summary quality” as the only cause of memory loss
- preserve enough event metadata that provenance, task identity, and key
  decisions survive compaction
- separate “active prompt window” from “durable session record”

## 10. Security And Sandbox

This area is less central to Holon right now, but still useful.

Relevant files:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/utils/sandbox/sandbox-adapter.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/tools/BashTool/shouldUseSandbox.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/tools/BashTool/`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/tools/PowerShellTool/`

Important observations:

- the sandbox is mainly command-execution-level OS sandboxing
- it is distinct from higher-level permission policy
- it is not the same as sandboxing the entire application

Design lesson for Holon:

- separate policy from sandboxing
- avoid pretending text filtering is the same as real execution isolation
- do not over-design this area before the runtime model is stable

## 11. KAIROS / Assistant Mode Direction

Claude Code appears to be moving toward a more persistent assistant runtime.

Relevant files:

- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/main.tsx`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/commands.ts`
- `/Users/jolestar/opensource/src/github.com/sanbuphy/claude-code-source-code/src/services/analytics/metadata.ts`

Important observations:

- `KAIROS` looks like a product-layer assistant mode
- `PROACTIVE` looks like a reusable autonomous-loop primitive
- channels, proactive behavior, background subagents, and brief delivery are
  converging toward one long-lived assistant runtime

Design lesson for Holon:

- build the runtime primitives first
- product modes should be layers on top, not the foundation

## 12. Recommended Reading Order For Holon Contributors

When working on `Holon`, use this order:

1. `docs/runtime-spec.md`
2. this document
3. the larger research notes:
   - `/Users/jolestar/opensource/src/github.com/jolestar/workspace/research/agent-tooling/Claude Code 源码分析.md`
4. the source files listed in the relevant section above

## 13. Concrete Guidance For Future Agents

When implementing or changing the Holon runtime:

- first ask which Claude Code pattern is relevant:
  - queue normalization
  - wake / sleep lifecycle
  - origin and trust modeling
  - background task rejoin
  - brief output
- then copy the abstraction, not the product surface
- avoid porting TUI-specific, enterprise-specific, or vendor-specific logic

If a design in `Holon` diverges from Claude Code, prefer the simpler runtime
model unless the missing complexity solves a concrete need.
