---
title: Create your first agent
summary: Install Holon, configure a model, and get a verified result from a named agent.
order: 20
aliases:
  - /guides/quick-examples
  - /guides/quick-examples.md
---

<a id="run-your-first-holon-task"></a>

<a id="create-your-first-agent"></a>

# Create your first agent

This walkthrough ends with a named agent answering one small request. Configure
a model before sending that request, then use the same agent from the terminal
or browser.

<a id="prerequisites"></a>

## Prerequisites

You need a terminal and access to a supported model provider. For a local
provider, its model server must be reachable. You do not need a template,
project workspace, or external event integration for this first task.

<a id="step-1-install-holon"></a>

## Step 1: Install Holon

<a id="option-a-homebrew-recommended"></a>

### Option A: Homebrew (recommended)

```bash
brew tap holon-run/tap
brew install holon
```

<a id="option-b-direct-binary-download"></a>

### Option B: Direct binary download

Download the binary for your operating system and architecture from
[GitHub Releases](https://github.com/holon-run/holon/releases/latest), make it
executable if needed, and place it on your `PATH`.

<a id="option-c-build-from-source"></a>

### Option C: Build from source

If you are changing Holon itself, follow the
[local build workflow](../maintainers/local-runtime.md).

<a id="verify-the-install"></a>

### Verify the install

```bash
holon --help
```

If the command is missing, fix your `PATH` before continuing.

<a id="step-5-configure-models"></a>
<a id="recommended-quick-setup-with-holon-onboard"></a>
<a id="7-set-up-api-credentials"></a>

<a id="step-2-configure-a-model"></a>

## Step 2: Configure a model

```bash
holon onboard
holon config doctor
```

The wizard selects a provider, configures credentials, and chooses a model.
Follow the [onboarding walkthrough](onboarding.md) if you need help with the
prompts. Resolve missing credentials or an unavailable model before proceeding;
never paste credentials into an agent conversation.

<a id="step-2-start-the-holon-runtime"></a>
<a id="option-b-daemon-mode-recommended-for-agents"></a>
<a id="4-run-as-a-background-daemon"></a>

<a id="step-3-start-the-runtime"></a>

## Step 3: Start the runtime

```bash
holon daemon start
holon daemon status
```

<a id="verify-the-daemon-is-running"></a>

### Verify the daemon is running

Check that status reports the expected running daemon. The daemon hosts agents
and their work while clients connect and disconnect.

<a id="step-4-create-an-agent"></a>
<a id="2-create-an-agent"></a>

<a id="step-4-create-a-named-agent"></a>

## Step 4: Create a named agent

Choose an unused ID; this example uses `first-helper`:

```bash
holon agent create first-helper
holon agent list
holon run --agent first-helper "Explain Rust ownership in three sentences. Do not change any files."
```

Confirm that the list contains `first-helper` and that the request produces an
answer. An agent appearing in the list proves creation, not that its model can
answer; the small request checks both. If it fails, inspect the error and run
`holon config doctor` again.

<a id="the-default-agent"></a>

### The default agent

A new runtime also provides `main`. You can use it instead of creating a named
agent. A named agent helps separate a continuing responsibility from other work;
see [Agents and guidance](../concepts/agents.md).

<a id="create-a-named-agent"></a>
<a id="list-agents"></a>
<a id="step-3-connect-with-the-tui"></a>
<a id="5-use-the-terminal-ui-tui"></a>

<a id="step-5-continue-in-a-client"></a>

## Step 5: Continue in a client

<a id="start-the-tui"></a>

### Start the TUI

```bash
holon tui
```

<a id="tui-basics"></a>

### TUI basics

Select `first-helper` before sending another request. Type `/` for the command
menu and `/help` for the current shortcuts. See [TUI reference](../reference/tui.md)
for navigation and commands.

<a id="basic-navigation"></a>
<a id="switch-agents-in-the-tui"></a>

You can also use the [Web GUI](../guides/use-web-gui.md). Confirm the selected
agent ID in either client so the follow-up goes to the same agent.

<a id="option-a-cli-mode-quick-start"></a>
<a id="1-one-shot-question"></a>

<a id="run-a-one-shot-question-instead"></a>

## Run a one-shot question instead

For a bounded request without a persistent daemon session:

```bash
holon run "Explain Rust ownership in three sentences"
```

A one-shot run may use several model/tool turns before it finishes. Do not use
it as a substitute for tracking work that must wait on CI or a later decision;
see [Run a long-lived task](../guides/run-long-lived-task.md).

<a id="stop-the-daemon"></a>

### Stop the daemon

After all work has finished, `holon daemon stop` stops the local daemon. Do not
stop it merely to close a client: other agents may still be working.

<a id="next-steps"></a>

## Next steps

The former quick-examples page now redirects here. Use the task-specific guides
below for work beyond the first successful request.

<a id="use-a-template"></a>
- [Create an agent from a template](../guides/create-agent.md) covers finding
  and preparing a role template before creating another agent.

<a id="3-switch-models"></a>
<a id="manual-setup-alternative"></a>
<a id="configuration-layers"></a>
<a id="set-provider-credentials"></a>
<a id="option-a-environment-variables-quick-start"></a>
<a id="option-b-credential-store-recommended-for-persistent-config"></a>
<a id="set-the-default-model"></a>
<a id="agent-level-model-override"></a>
<a id="verify-model-configuration"></a>
<a id="8-inspect-configuration"></a>
- [Configure models](../guides/configure-models.md) covers manual credentials,
  per-agent model choices, fallback, and configuration inspection.

<a id="remote-tui-optional"></a>
- [Connect to a remote runtime](../guides/connect-remote-runtime.md) covers
  access modes, authentication, and remote clients.

<a id="6-start-the-http-server"></a>
- [Automate over HTTP](../guides/automate-over-http.md) covers submitting and
  following work through the API.

<a id="9-run-a-multi-turn-task"></a>
<a id="10-create-an-agent-with-a-custom-workspace"></a>
- [Work with workspaces](../guides/work-with-workspaces.md) covers project
  selection and isolated changes. [Run a long-lived task](../guides/run-long-lived-task.md)
  covers goals that need durable progress and waits.

<a id="see-also"></a>

<a id="troubleshooting"></a>

## Troubleshooting

<a id="daemon-wont-start"></a>

### Daemon won't start

Check `holon daemon status` and `holon daemon logs`. Resolve the reported startup
error rather than deleting runtime state or repeatedly starting another daemon.

<a id="tui-cant-connect"></a>

### TUI can't connect

Confirm that the daemon is running on the host you intend to use. For a remote
connection, follow the remote guide above rather than assuming the local socket
can reach another machine.

<a id="model-errors"></a>

### Model errors

Run `holon config doctor`. Check the chosen model and credentials with the
model-configuration guide above. See [Troubleshooting](../guides/troubleshooting.md)
for diagnosing a request that was accepted but failed or stopped making progress.
