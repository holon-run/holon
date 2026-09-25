---
title: Configure advisory decisions
summary: Set up a dedicated Decision provider and enable the AdvisoryDecision tool to give agents non-authoritative second opinions.
order: 36
---

# Configure Advisory Decisions

Complex workflows often force agents to make judgment calls between competing
approaches. The Decision subsystem gives agents a way to request a structured
second opinion through the `AdvisoryDecision` tool without granting that advisor
any execution authority.

This guide walks through choosing a decision provider, configuring model
routes, and setting call guardrails.

## Prerequisites

- A running Holon daemon (v0.45.0 or later).
- A configured primary model for your agent.
- For local decision inference: an installation with the `local-onnx` feature
  (included in official release binaries).
- For remote decision inference: an API key or endpoint for a provider that
  advertises Decision capability (such as TypeSafe Jev or an OpenAI-compatible
  route).

## Step 1: Choose a Decision Provider

Holon supports two provider architectures for decisions:

1. **Local ONNX (zero-egress):** Runs a compact classifier entirely on your CPU
   using ONNX Runtime. No prompts or decisions leave your machine.
2. **Remote Provider:** Sends structured decision queries to an external model
   endpoint over HTTP (such as TypeSafe Jev or OpenAI-compatible endpoints).

For private environments or air-gapped tasks, choose the local ONNX provider.

## Step 2: Configure the Provider

### Option A: Use the Local ONNX Provider

Enable the local provider and select a preset:

```bash
holon config set decision.enabled true
holon config set decision.local_onnx.enabled true
holon config set decision.local_onnx.preset "jev-selector-q4f16"
```

You can allocate additional CPU threads if your system has spare cores:

```bash
holon config set decision.local_onnx.num_threads 2
```

### Option B: Use a Remote Provider

Set the decision model route directly:

```bash
holon config set decision.enabled true
holon config set decision.model "typesafe@default/typesafe-ai/jev"
```

You can also configure these settings visually in the Web GUI under **Settings**
→ **Decision Settings**.

## Step 3: Enable the Advisory Tool and Set Guardrails

The `AdvisoryDecision` tool remains hidden from agents until you explicitly
turn it on. Enable the tool and set safety boundaries to prevent runaway loops:

```bash
# Expose the tool to agent execution loops
holon config set decision.tools.enabled true

# Cap tool invocations to 3 calls per agent turn
holon config set decision.tools.max_calls_per_turn 3

# Require at least 65% confidence; lower scores result in an explicit abstain
holon config set decision.tools.min_confidence 0.65

# Set an execution timeout (in milliseconds)
holon config set decision.tools.timeout_ms 10000
```

## Step 4: Verify with a Test Prompt

Run a short prompt that requires choosing between explicit options:

```bash
holon run "Evaluate whether to use an index scan or table scan for 50 rows in PostgreSQL. Consult the advisory decision tool before recommending."
```

Inspect the output or run transcript:

```bash
holon transcript
```

You will see an `AdvisoryDecision` tool invocation containing:
- `question`: The specific evaluation prompt.
- `options`: The candidate options evaluated.
- `outcome`: Either `select` (with recommended choice and confidence score) or
  `abstain` (if confidence fell below `min_confidence`).

Notice that the agent receives the outcome as advisory evidence. It remains
free to accept, weigh, or reject the recommendation.

## Next Steps

- [Configuration reference](/reference/configuration.md) — All `decision.*` configuration keys.
- [Model tool schema inventory](/reference/model-tool-schema-inventory.md) — The machine-readable schema for `AdvisoryDecision`.
- [Web GUI guide](/guides/use-web-gui.md) — Manage decision settings and view telemetry from the browser.
