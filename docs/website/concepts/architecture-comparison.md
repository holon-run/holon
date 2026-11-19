---
title: Architecture comparison
summary: How Holon's self-owned Rust agent runtime compares with CLI-orchestrated self-hosted AI assistants, embedded harnesses, and cloud bot platforms.
order: 160
---

# Architecture comparison: self-hosted AI assistants vs. agent runtimes

If you are evaluating **self-hosted AI assistants** (including open-source alternatives to Grok Bot or OpenClaw) or comparing **agent runtimes**, most tools fall into three different architectural layers. Comparing them on surface features alone—such as whether they bundle voice or browser panels—hides the core question: **who owns task execution, waits, isolation, and trust boundaries?**

## Three architectural layers

| Category | Examples | Who owns the agent loop | Primary strength | Trade-off |
|---|---|---|---|---|
| **Self-owned agent runtime** | **Holon** (Rust daemon) | Holon itself (`holon`) | Persistent `WorkItem`s, event-driven wait/wake, linked `git worktree` isolation, explicit origin/trust boundaries, model-agnostic routing | Smaller ecosystem connector surface today (no built-in MCP client, voice, or bundled browser) |
| **Orchestration shells over external coding CLIs** | OpenMausBot, Codync, `nightly-labs/OpenBot` | External CLIs (`Claude Code`, `Codex`, `Cursor`, `OpenCode`, etc.) | Inherit existing CLI subscriptions, skills, and MCP servers; strong desktop/mobile/voice UX and shared chat surfaces | Execution lifecycle, recovery, and permission boundaries depend on the wrapped CLI |
| **Embedded-runtime apps & closed cloud bots** | Rakazo (embeds Pi), Grok Bot (xAI closed cloud) | Embedded JS runtime or vendor cloud host | Fast consumer onboarding; cloud workspaces, built-in voice, or mobile per-action UI | Single-vendor lock-in (cloud) or Docker/harness coupling without a Rust daemon |

## Where Holon differs in runtime behavior

Surface feature tables usually ask whether a tool has a desktop shell, mobile client, or scheduled routines. For long-running work on your own computer or server, the harder failures happen at the runtime layer:

1. **Work survives client disconnects and restarts.** Clients (Web GUI, TUI, macOS menu bar app, iOS/Android remote clients) connect to one background `holon` daemon. Tasks (`TaskHandle`) and durable objectives (`WorkItem`) live in runtime-managed SQLite state rather than inside a browser tab or terminal multiplexer.
2. **Explicit wait and wake semantics.** Agents pause cleanly on `WaitFor` conditions (`task_result`, `timer`, `external`, `operator_input`) instead of polling in a loop. An incoming webhook or schedule acts as a wake hint that triggers re-verification—external triggers do not automatically grant operator authority.
3. **Origin and trust separation.** The runtime distinguishes operator instructions, agent-authored guidance, tool results, child agent reports, and external webhook/app payloads. Untrusted external text read during a task cannot self-escalate into operator commands.
4. **Workspace and parallel branch isolation.** Each agent separates its persistent `agent_home` (identity, memory, notes, private state) from attached project workspaces. Parallel work on a repository uses registered linked `git worktree` roots (`CreateWorktree` / `RemoveWorktree`) with active occupancy checks so concurrent tasks do not overwrite each other.
5. **Model-agnostic local and cloud routing.** Holon runs against OpenAI-compatible endpoints, Anthropic, Gemini, and local inference servers such as Ollama or `llama.cpp`, without requiring a specific vendor coding CLI subscription.

## Surface capabilities and shipped boundaries

When comparing Holon with Grok Bot or CLI-orchestrated self-hosted assistants, evaluate both what is shipped and what is intentionally absent today:

| Dimension | Holon today (verified in repository) | Notes for evaluators |
|---|---|---|
| **Runtime & deployment** | Rust binary (`holon`), local daemon, Docker image | Single-host daemon; Linux amd64, macOS arm64/amd64 |
| **Clients** | Web GUI, TUI, CLI, macOS menu bar (`HolonMenu`), iOS & Android remote apps | Mobile apps are remote clients connected to your daemon, not on-device agents |
| **Multi-agent & templates** | 25+ checked-in `agent_templates`, `InvokeAgent` supervised children, peer `SendAgentMessage` | Role-scoped `skills/`, separate `agent_home`, explicit cleanup rules |
| **Routines & external events** | Persistent timers (`interval_ms` / `calendar_expression`), webhook ingress, `AgentInbox` | Waking an agent never bypasses trust or verification checks |
| **Host execution** | Workspace-scoped shell commands, patches, background `command_task`s | Host-local execution (`host_local`), not GUI desktop mouse/screen automation or VM-per-action |
| **MCP support** | **Not built in** | External CLI tools are wrapped via `skills/`; no native MCP client or server yet |
| **Live voice (TTS/STT)** | **Not built in** | Text/media/artifact workflows only |
| **Integrated browser** | **Not built in** | No bundled headless browser engine inside the runtime; external browser skills are optional |
| **Desktop shell coverage** | macOS menu bar app + cross-platform Web GUI & TUI | No native Windows or Linux Electron/desktop wrapper |

## Choosing the right fit

- **Choose Holon** if you want a self-hosted, model-agnostic AI assistant workbench on your own machine or server where agents track multi-turn work items, wait on CI/timers/webhooks without burning loops, isolate parallel repo edits in git worktrees, and enforce explicit trust boundaries.
- **Choose a CLI orchestration shell** (such as OpenMausBot, Codync, or `nightly-labs/OpenBot`) if your primary goal is putting a chat/voice/mobile UI over existing Claude Code, Codex, or Cursor CLI sessions and immediately reusing their MCP ecosystems.
- **Choose a hosted cloud bot** (such as Grok Bot) if you prefer zero local setup and vendor-hosted execution over self-hosting, local repository access, and model choice.

## Next reading

- [Runtime model](./runtime-model.md) — how the daemon, clients, agents, work items, and workspaces fit together
- [Work items and tasks](./work-items-and-tasks.md) — durable objectives vs. supervised execution handles
- [Events, waits, and wakeups](./events-and-waits.md) — how agents pause and resume on timers, tasks, and external wakes
- [Trust boundaries](./trust-boundaries.md) and [Security and execution boundaries](./security-and-execution-boundaries.md) — authority separation and host-local limits
- [Verify lifecycle guarantees yourself](../guides/verify-lifecycle.md) — a hands-on reproduction of the restart, wake, isolation, and trust behavior described on this page
- [Getting started](../getting-started/) — install Holon and run your first local assistant
