---
title: "From prompt to finished work: a practical Holon workflow"
summary: "A repeatable workflow for giving an agent a real repository task, letting it work past the prompt, and checking the result when it comes back."
order: 50
---

# From prompt to finished work: a practical Holon workflow

<img src="/assets/holon-agent-workspace-cover.webp" width="1672" height="941" alt="A Holon agent workspace showing ongoing work, progress, and a result ready to inspect." decoding="async" fetchpriority="high">

> This is a practical workflow, not a claim that an agent can replace review. You still decide what the agent may change and whether the result is acceptable.

Most agent demos end when the model produces a confident answer. Real work usually starts there.

You still need to inspect the repository, change the right files, run checks, wait for a command or a review, and decide whether the result is ready to keep. If the terminal closes halfway through, you need a way to return to the same task without reconstructing the entire conversation.

Holon is designed for that gap. It is a local workbench for agents handling ongoing work. The runtime keeps an agent, its workspace, and its tracked work available while a task runs, waits, or asks for your input.

This guide walks through one small, repeatable workflow:

1. choose a bounded repository task;
2. start Holon in durable mode;
3. create an agent and a WorkItem;
4. let the agent work while you disconnect;
5. return to the workspace and check the evidence.

The important outcome is not a longer transcript. It is a result you can inspect.

## Start with work that has a finish line

Your first durable task should be real, but small enough that you can recognize a correct result. Good candidates include:

- adding a validation message and its tests;
- updating a documented command and checking the links;
- fixing a narrow bug with an existing regression test;
- reviewing a pull request and recording the remaining issues.

Avoid starting with “improve the whole codebase” or an open-ended research project. A durable runtime can preserve a large task, but it cannot make an unclear outcome easier to review.

For the example below, imagine a repository where you want to improve the error shown for an invalid configuration value. The exact files will differ in your project. The boundary should not:

```text
Inspect the current handling of invalid configuration values.
Create a tracked work item for the change.
Make the smallest useful implementation and add or update a focused test.
Run the relevant checks.
Do not do unrelated cleanup.
Finish with the files changed, checks run, and anything that still needs my decision.
```

That prompt gives the agent a destination and a stopping rule. It does not ask the model to pretend the work is complete before it has inspected the repository.

## Choose one-shot or durable execution

Holon supports a quick one-shot command:

```bash
holon run "What is Holon?"
```

That mode is useful when the task can finish in one turn and does not need a saved lifecycle. Use the daemon when the work may span commands, sessions, waiting conditions, or human input:

```bash
holon daemon start
holon daemon status
```

The daemon keeps the runtime available independently of your TUI connection. Your terminal is a way to interact with the work, not the work itself.

This distinction is worth making explicit. A disconnected terminal does not mean that every possible task will continue. The daemon, model provider, workspace, and permissions still need to be configured and available. It does mean that a normal TUI disconnect is no longer the same thing as abandoning the task.

## Give the work an agent and a WorkItem

For a focused role, create a named agent from an installed or synced template:

```bash
holon agent create builder --template software-developer
```

Then connect to the runtime:

```bash
holon tui
```

In the TUI, send the task as work you want tracked, not merely as a question:

```text
I need to improve the invalid-configuration error in this repository.
Create a WorkItem with a short plan and acceptance checks.
Inspect the current behavior, make the smallest change, run the focused tests,
and stop for my input if the scope or implementation choice is unclear.
```

A **WorkItem** is Holon's durable unit of tracked work. It holds an objective, a plan, progress, waiting state, and completion criteria. That gives the agent something more useful than a prompt to return to after a command finishes or a person reconnects.

You do not need to create a WorkItem for every question. The website reference recommends one when work spans multiple turns, needs resumable progress, waits on external state, or has acceptance criteria worth tracking. For a quick explanation, `holon run` is usually the simpler choice.

## Leave the terminal without losing the task

Once the agent has created the WorkItem and started execution, you can disconnect the TUI:

```text
Ctrl+C
```

The daemon and the agent continue independently of that connection. Later, check the named agent from another terminal:

```bash
holon agent status builder
```

Or reconnect to the TUI:

```bash
holon tui
```

The useful question is not “Did the model send another message?” It is “What state is the work in now?” The agent may be editing files, waiting for a command, waiting for your decision, or waiting for an external event.

## Let waiting be part of the workflow

Long tasks rarely run as one uninterrupted model turn. A build may take time. A design choice may need your approval. CI may need to report back.

Holon represents those as explicit waiting conditions.

### A command result

When the agent runs a build, test, or lint command, the command can continue as a background task while the agent sleeps or does other work:

```text
Run the focused tests and wait for the result before writing the completion summary.
```

The important detail is that the test result becomes part of the work's state. “I think the tests pass” is not equivalent to a recorded command result.

### A decision from you

If the agent finds two reasonable approaches, it can leave the WorkItem needing input instead of silently choosing one:

```text
I found two ways to handle the invalid value:
Option A keeps the existing error type and improves its message.
Option B introduces a new error variant.
Which boundary should I preserve?
```

You can respond later through the TUI, CLI prompt, or HTTP API. The decision is attached to the ongoing work rather than lost in a closed terminal.

### An external event

For a CI run, webhook, or scheduled event, Holon can wait on an external trigger:

```text
The branch is ready. Wait for the CI check before recommending the next step.
```

The runtime records what it is waiting for and why. When the configured event wakes the agent, the agent can inspect the new state and continue—or update the WorkItem if the check failed.

## Come back to evidence, not confidence

When the agent says the task is complete, inspect the result in the repository and the work record. A useful review asks:

- Which files changed?
- Did the change stay within the requested scope?
- Which command or test actually ran?
- Did the command finish successfully?
- Is there a remaining decision, failed check, or known limitation?
- Can another person understand what happened from the WorkItem and completion brief?

Holon’s completion brief is intended to make that last handoff explicit. The long-lived task guide shows a brief containing the objective, changes, and verification result. You can also inspect the transcript and current agent status:

```bash
holon transcript
holon agent status builder
```

The final check still belongs to you. Open the diff. Read the relevant test. Run an additional check when the change deserves one. A completion brief is evidence about the work; it is not a substitute for engineering judgment.

For repeatable work, ask for the same small handoff every time:

```text
Result: one sentence describing the outcome.
Changed: files or external records touched.
Checked: commands that ran and their results.
Waiting: the next decision, event, or follow-up, if any.
```

That format helps you scan several completed tasks without rereading every model turn. It also gives the next person a clear place to start when the work needs another pass.

## What this workflow does—and does not—promise

This workflow is useful when the prompt is only the beginning of the job. It gives the task a durable home, makes waiting visible, and preserves a path back to the workspace after a disconnect.

It does not guarantee that the agent chose the right implementation. It does not grant permissions the agent was not configured to have. It does not turn an external system into a trusted input, and it does not merge code or approve a release on your behalf. If you expose a runtime beyond the local machine, follow the deployment and access guidance rather than treating durability as isolation.

Start with a task whose acceptance check you can name. Keep the workspace and permissions narrow. Ask the agent to stop when it needs a decision. Then inspect the result that comes back.

## Your next step

Install Holon, configure a model provider, and create your first agent with the [getting started guide](/getting-started/first-agent). When the task needs to survive a disconnect or wait for a result, follow [Run a long-lived task](/guides/run-long-lived-task) and adapt the prompt above to your repository.
