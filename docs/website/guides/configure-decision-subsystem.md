---
title: Configure advisory decisions
summary: Choose a Decision route, set call limits, and verify a recommendation or explicit abstention.
order: 140
---

# Configure advisory decisions

`AdvisoryDecision` asks for a non-authoritative second opinion between explicit
options. Its result cannot grant permission or approve an operation. For the
routing model, see [Models and providers](/concepts/models-and-providers.md).

## Prerequisites

- A running daemon and a working primary model; see
  [Configure models](/guides/configure-models.md).
- Permission to change shared Decision settings.
- For local inference, a build with the `local-onnx` feature and verified model
  files. Downloading a preset needs network access; local inference does not
  send decision prompts to a remote provider.
- For remote inference, credentials and access to a route explicitly declaring
  Decision capability and a supported `decision_protocol`.

## Step 1: Choose a Decision Provider

Inspect the existing selection before changing it:

```bash
holon config get decision.enabled
holon config get decision.model
holon config get decision.local_onnx.enabled
holon config models list
```

Choose one of the following paths. A remote `decision.model` and enabled local
ONNX cannot be configured together. Ordinary chat capability alone is not
sufficient for a Decision route.

## Step 2: Configure the Provider

### Option A: Use the Local ONNX Provider

In the Web GUI, open **Settings → Decision Settings**, choose the
`jev-selector-q4f16` preset, and click **Download and verify preset**. Wait
until it reports ready before enabling local inference. For a pre-provisioned
model, use `decision.local_onnx.model_dir` instead; the files must match the
supported model/manifest format.

Then set the local route:

```bash
holon config unset decision.model
holon config set decision.local_onnx.preset jev-selector-q4f16
holon config set decision.local_onnx.num_threads 2
holon config set decision.local_onnx.enabled true
holon config set decision.enabled true
```

If switching back from a custom model directory to the managed preset, unset
`decision.local_onnx.model_dir` so it does not override the preset location.
The managed preset is stored under
`<HOLON_HOME>/models/decision/local-onnx/<preset>/`.

### Option B: Use a Remote Provider

Authenticate the provider using the [model setup guide](/guides/configure-models.md).
For the built-in TypeSafe Jev route, use:

```bash
holon config set decision.local_onnx.enabled false
holon config set decision.model "typesafe@default/jev-latest"
holon config set decision.enabled true
```

For a custom service, configure its endpoint and credential source first.
Register the service's actual model under `models.catalog` with
`capabilities.decision: true` and its documented protocol: `jev`,
`cloudflare_clef`, `openai_decisions`, or `openai_compatible`. Preserve existing
catalog entries when editing that object. Then select the corresponding
`provider@endpoint/model` route with `decision.model`.

These protocol names describe Holon's adapters; they do not establish that a
particular model or endpoint is available to your account. In particular,
`openai_decisions` uses the dedicated `/v1/decisions` endpoint through the
API-key-based `openai_responses` transport, not an OAuth Codex transport.
Exact metadata fields are in the
[Configuration reference](/reference/configuration.md#model-catalog-overrides-modelscatalog).

## Step 3: Enable the Advisory Tool and Set Guardrails

```bash
holon config set decision.tools.enabled true
holon config set decision.tools.max_calls_per_turn 3
holon config set decision.tools.min_confidence 0.65
holon config set decision.tools.timeout_ms 10000
```

The tool stays hidden until enabled. These settings cap calls, require a
minimum confidence, and bound the tool's wait. Provider-level timeout,
concurrency, and queue settings are separate; see the
[Decision key table](/reference/configuration.md#decision-subsystem-settings).

Read configuration-change notes on stderr. If they require a restart, let
active work settle and restart through your installation's normal procedure
before testing. Saving a key does not prove the running daemon has applied it.

## Step 4: Verify with a Test Prompt

Use an existing test agent; replace `decision-test` with its ID:

```bash
holon config doctor
holon run --agent decision-test "Call AdvisoryDecision once with question 'Which review step should come first?', options ['Read the diff', 'Publish a review'], and state 'The diff has not been inspected; publishing is not authorized.' Report the returned outcome and reason. Do not publish anything or change files."
holon transcript --agent decision-test --limit 30
```

## Confirm it worked

The transcript contains an actual `AdvisoryDecision` call and its result.
Inspect `outcome`, `choice`, `confidence`, `abstain`, `reason`, and provider/model
metadata. A valid result can select an option, report a provider fallback, or
abstain. An explicit abstention verifies the failure path; it does not prove
successful inference. Record its reason rather than presenting it as a choice.

If the agent never called the tool, the test has not verified the Decision
route. The recommendation remains evidence for the agent to weigh, not
approval to publish or change state.

## Troubleshooting

- **Tool absent:** check both `decision.enabled` and `decision.tools.enabled`,
  the effective configuration, and the agent's allowed tools.
- **Conflicting routes:** unset `decision.model` for local ONNX, or disable
  local ONNX for a remote model.
- **Missing preset or unsupported build:** check the GUI download status and
  the build's `local-onnx` support. Do not assume selecting a preset downloads it.
- **Provider unavailable or rejected:** check credentials, route capability,
  protocol, endpoint access, and any required daemon restart.
- **Abstention:** inspect the reason for timeout, provider failure, call limit,
  invalid choice, or low confidence. Do not lower confidence limits merely to
  force a recommendation.

To hide the tool without deleting the provider configuration:

```bash
holon config set decision.tools.enabled false
```

## Next Steps

- [Configuration reference](/reference/configuration.md) — exact configuration keys.
- [Model tool schema inventory](/reference/model-tool-schema-inventory.md) — tool schema.
- [Web GUI guide](/guides/use-web-gui.md) — settings and telemetry.
