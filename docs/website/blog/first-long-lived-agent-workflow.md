---
title: "Build Your First Long-Lived Agent Workflow with Holon"
summary: "A practical path from installation to a named agent, real workspace, explicit WorkItem, and human approval."
order: 30
---

# Build Your First Long-Lived Agent Workflow with Holon

A long-lived agent does not need to begin with a complex automation system. The fastest way to understand the model is to give one agent a small responsibility that naturally crosses a decision boundary.

In this guide, you will:

1. install and configure Holon;
2. start the local daemon;
3. create a named agent;
4. attach a real repository as its workspace;
5. ask the agent to create a WorkItem and inspect a small improvement;
6. require it to wait for your approval before editing;
7. disconnect, return later, approve the change, and receive a final brief.

This demonstrates the essential lifecycle without requiring a webhook, CI integration, or public server.

**Time:** about 10–15 minutes.  
**Changes:** the final step may modify one small file in the repository you choose. Use a disposable repository or clean working tree if you do not want to affect active work.

## Prerequisites

You need:

- macOS or Linux with a terminal;
- a model-provider account supported by Holon;
- a small local Git repository you are comfortable inspecting and, optionally, editing;
- Homebrew for the shortest install path, or a Holon release binary.

Holon is early-stage software. Check the current release notes and documentation if a command differs from the examples below.

## 1. Install Holon

With Homebrew:

```bash
brew tap holon-run/tap
brew install holon
```

Verify that the CLI is available:

```bash
holon --help
```

If you do not use Homebrew, download the current binary from GitHub Releases or build the repository with Cargo. The Getting Started documentation lists the available release targets.

## 2. Configure a provider

Run the interactive onboarding flow:

```bash
holon onboard
```

The wizard guides you through provider selection, credential entry, default-model selection, and optional search configuration.

Prefer the credential flow offered by `holon onboard`. Do not paste API keys into prompts, repositories, screenshots, or public issue reports.

After onboarding, check the configuration:

```bash
holon config get model.default
holon config doctor
```

If `config doctor` reports an error, fix that before continuing. The agent cannot perform useful work without a working model configuration.

## 3. Start the durable runtime

One-shot commands are useful for quick tasks, but durable work requires the daemon:

```bash
holon daemon start
holon daemon status
```

The daemon keeps agent state, queues, WorkItems, and waits available independently of the terminal UI. You can close a client and reconnect without making the current conversation the sole owner of the work.

To stop it later:

```bash
holon daemon stop
```

Keep it running for the rest of this guide.

## 4. Prepare a small repository

You can use an existing clean repository. If you want a disposable example, create one:

```bash
mkdir -p ~/tmp/holon-first-workflow
cd ~/tmp/holon-first-workflow
git init
printf '# Demo project\n\nA small repository for testing a durable agent workflow.\n' > README.md
git add README.md
git commit -m 'docs: initialize demo project'
```

If Git asks for your name or email, configure them locally or use another repository that already has a commit.

Check the working tree before giving it to an agent:

```bash
git status --short
```

For this guide, the safest starting point is no output: a clean working tree.

## 5. Create a named agent

Create a stable agent for this responsibility:

```bash
holon agent create maintainer
holon agent list
```

The agent has its own Agent Home, role instructions, history, and durable work state. Naming it `maintainer` is only an example; choose a role that describes the responsibility you want to keep.

## 6. Attach the repository as the agent's workspace

Run:

```bash
holon workspace attach --agent maintainer ~/tmp/holon-first-workflow
```

If you chose another repository, replace the path.

A workspace is more than a shell `cd`. It defines the execution root where the agent reads files, resolves workspace instructions, runs commands, and applies changes. The binding remains available across sessions.

## 7. Start a WorkItem through the TUI

Open the terminal interface:

```bash
holon tui
```

Select the `maintainer` agent, then send this prompt:

```text
Inspect the current repository and create a WorkItem to improve the README for a first-time contributor.

First, read the repository and propose one small, verifiable documentation change. Record a short plan and completion criteria. Do not edit files yet. Ask for my approval and wait.
```

The exact wording is not special. It makes the desired lifecycle explicit:

- the objective should be durable;
- the agent must inspect the real workspace;
- the proposed change should be small and verifiable;
- editing is not authorized yet;
- the next state should be a wait for operator input.

Review what the agent reports. It should identify the repository state, create or anchor the work in a WorkItem, propose a bounded change, and stop at the approval boundary.

## 8. Disconnect while the responsibility remains

Exit the TUI with `Ctrl+C`.

The client has disconnected, but the daemon and durable agent state remain. From another terminal, inspect the agent:

```bash
holon agent status maintainer
```

Then reconnect:

```bash
holon tui
```

Select `maintainer` again. The useful question is not whether every line of chat is visible. It is whether the agent can identify the same objective, explain what it is waiting for, and continue from the corresponding WorkItem.

## 9. Approve the bounded change

If the proposal is acceptable, reply:

```text
Approved. Make only the proposed README change, verify the final diff and repository status, then complete the WorkItem with a concise brief. Do not commit.
```

If it is not acceptable, change the constraints instead of approving:

```text
Do not make that change. Revise the plan so it only adds a short “How to run” section and does not change existing wording. Wait for approval again.
```

This is part of the workflow, not a failure. A durable agent should preserve the work while the operator changes the authorized plan.

## 10. Inspect the result

After completion, check the repository yourself:

```bash
cd ~/tmp/holon-first-workflow
git diff -- README.md
git status --short
```

A useful final brief should tell you:

- what changed;
- which file was modified;
- what verification was run;
- whether anything failed;
- whether the WorkItem is complete;
- that no commit was created, because you did not authorize one.

The internal execution may include several reads and checks. The user-facing brief should preserve the decision-relevant result without requiring you to inspect every tool call.

## What you just tested

This small workflow exercises the core pieces of long-lived agent work:

### Stable identity

The `maintainer` agent persists independently of one terminal session.

### Real workspace

The agent acts on a repository you selected, using its files and local tool environment.

### Durable objective

The WorkItem holds the responsibility, plan, progress, waiting state, and completion boundary.

### Explicit human control

The agent can inspect and propose before it is allowed to edit. Your later input changes the work from waiting to runnable.

### Resume and delivery

Closing the TUI does not redefine the task. Reconnecting lets the agent resume the same work and eventually deliver a concise brief.

## From operator waits to external events

This guide uses operator approval because it is safe and easy to observe. The same runtime model can represent other waits:

- a background build or test task completes;
- CI changes state;
- a pull request receives a review;
- a timer reaches a scheduled check;
- an approved external integration sends an event.

External wake integrations require deliberate configuration. Treat callback endpoints, access tokens, and other capability URLs as secrets. Do not publish them in tutorials, screenshots, repositories, or social posts.

## Next experiments

Once the basic lifecycle works, replace the demo task with one real responsibility:

- follow a pull request until CI and review are resolved;
- investigate an issue and wait for a reproduction or log bundle;
- prepare a release and wait for human approval before publishing;
- monitor an operational question and resume when a known external object changes.

Keep the completion condition measurable. Start with one agent and one responsibility before adding roles or integrations.

Holon's core idea is not that the agent should act forever. It is that responsibility should remain available when action is temporarily impossible.

**Next:** Read the [Durable Agent Workflow](/guides/durable-agent-workflow), [WorkItems](/guides/work-items), [Workspaces](/guides/workspaces), and [Trust Boundaries](/concepts/trust-boundaries) guides to extend this example safely.
