# Model Route Syntax & Inspection

## Canonical Route Reference Format

Explicit model overrides across Holon tools and CLI commands use the canonical route reference syntax:

```text
provider@endpoint/model
```

Examples: `openai@responses/gpt-5.4`, `anthropic@messages/claude-sonnet-4-6`, `google@gemini/gemini-2.5-pro`.

## Tool Override Discipline

- Leave `model` / `provider` override fields unset on `ViewImage`, `GenerateImage`, `CreateAgent`, `InvokeAgent`, `WebSearch`, and `WebFetch` unless the operator or task explicitly requests a specific route or provider.
- Explicit model/provider overrides apply only to that call or target agent and **never silently fall back** to another route on failure. When an explicit override fails, inspect the structured tool error (`details` / `recovery_hint`) or report the exact blocker.

## CLI Inspection & Configuration Commands

When `ExecCommand` is available, inspect or manage configured providers, models, and agent model overrides through the `holon` CLI:

- `holon config providers list` — list configured model providers.
- `holon config providers get <provider>` — inspect non-secret configuration for one provider.
- `holon config providers doctor <provider>` — validate provider connectivity and configuration health.
- `holon config models list` — list configured/cached provider models, media `view_image`/`generate_image` capabilities, and canonical route refs.
- `holon config models refresh --provider <provider>` — refresh cached model catalog entries for a provider.
- `holon config web-providers` — list effective `WebSearch` and `WebFetch` providers and extraction modes as JSON.
- `holon agent model get [agent_id]` — inspect the effective model and override settings for an agent.
- `holon agent model set <model> [agent_id]` — set an agent's persistent model override (optional `--reasoning-effort`, `--service-tier`).
- `holon agent model clear [agent_id]` — remove an agent's persistent model override.
