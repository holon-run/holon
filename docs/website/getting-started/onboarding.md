---
title: Onboarding guide
summary: Interactive setup with `holon onboard` — provider, credential, model, and search configuration.
order: 30
---

# Onboarding Guide

`holon onboard` is the fastest way to set up Holon. It launches an interactive
Terminal UI wizard that walks you through provider selection, credential entry,
model choice, and search configuration, then writes the results into your Holon
config and credential store.

## When to use onboarding

- **First-time setup** — you installed Holon and need to configure a model
  provider before creating an agent.
- **Provider switch** — you want to change your default provider or model and
  prefer a guided flow over manual config edits.
- **Credential repair** — your saved credential has expired or is invalid, and
  Holon detects it needs attention.

## Quick start

```bash
holon onboard
```

This launches the interactive TUI. At the end you will have a working default
model configuration — no manual config file edits needed.

## The onboarding flow

The wizard has five steps. Use arrow keys to navigate, Enter to select, and
Esc to go back. Each choice is confirmed before the next step appears.

### 1. Provider selection

Pick your model provider from the built-in list:

- **Anthropic** — Claude models via Anthropic Messages API (API key)
- **OpenAI** — GPT and o-series models via OpenAI API (API key)
- **OpenAI Codex** — Codex-hosted models (browser OAuth login)
- **DeepSeek** — DeepSeek models (API key)
- **Gemini** — Google Gemini models (API key)
- **Custom provider** — any OpenAI-compatible endpoint

The wizard only shows providers that are not yet configured. If you already
have one provider configured and want to switch, the wizard shows that provider
as an update candidate.

### 2. Credential entry

The credential step depends on your provider:

- **API key providers** — Enter your key. Input is never echoed to the screen
  and never stored in plaintext config files.
- **OpenAI Codex (OAuth)** — The wizard opens your browser for OAuth login,
  then captures the resulting credential automatically.
- **No-credential providers** — Skipped; the provider is configured without
  authentication.

Credentials are stored in the Holon credential store at
`~/.holon/credentials.json`, not in `config.json`. This keeps secrets out of
config files and shell history.  The store is a single encrypted JSON file.

### 3. Model selection

Choose your default model. The wizard lists the provider's commonly used
models with a short description. You can also type a custom model ID if your
preferred model is not in the list.

The selected model becomes `model.default` in your config. You can override it
per-agent later with `holon agent model set`.

### 4. Search configuration

Holon agents can use WebSearch as a built-in tool. The wizard offers three
search modes:

| Mode | Behavior |
|------|----------|
| **Disabled** | No web search capability |
| **Auto** | Prefer model-native search if available, fall back to managed DuckDuckGo |
| **Managed (DuckDuckGo)** | Use Holon's built-in DuckDuckGo search provider |

Search configuration can be changed later with `holon config set`.

### 5. Review and apply

The wizard shows a summary of all your choices before applying. Confirm to
write the configuration and credential store. Holon prints a confirmation
summary when done.

## After onboarding

Once onboarding completes, your Holon config is ready:

```bash
# Verify the configuration
holon config doctor

# See the default model
holon config get model.default

# Start the daemon and create your first agent
holon daemon start
holon agent create my-first-agent
```

Continue to [Create your first agent](first-agent.md) for the full walkthrough.

## Credential repair

If your saved credential stops working — for example, an API key expires or
an OAuth token is revoked — Holon detects this at startup and may suggest
re-running onboarding:

```bash
holon onboard
```

The wizard shows which provider has the credential issue and guides you
through updating it. Existing configuration for other providers is left
untouched.

When the credential repair is for a provider that was already configured, the
wizard requires confirmation before overwriting — it shows the affected
provider and asks you to confirm the update.

## Non-interactive diagnostics

If you want to inspect onboarding status without entering the interactive
wizard, use `--json`:

```bash
holon onboard --json
```

This prints a machine-readable onboarding report with each section's status:

- `configured` — already set up and working
- `missing` — not yet configured
- `unavailable` — provider is not reachable
- `restricted` — partially configured but needs attention
- `skipped` — intentionally skipped or not applicable
- `failed` — configuration attempt failed and needs repair

Each section includes a `summary` string, optional `details`, and
`actions` with suggested CLI commands.

## Configuration files

| File | Purpose |
|------|---------|
| `~/.holon/config.json` | Provider definitions, model defaults, search settings |
| `~/.holon/credentials.json` | Encrypted credential profiles (API keys, OAuth tokens) |

Onboarding writes to both. You should not edit credential files directly;
use `holon onboard` or `holon config credentials` instead.

## CLI reference

```
holon onboard [--json]
```

| Flag | Description |
|------|-------------|
| `--json` | Print onboarding diagnostics as JSON and exit (non-interactive) |

## See also

- [Create your first agent](first-agent.md) — full walkthrough from install to first prompt
- [Configuration reference](/reference/configuration.md) — config file schema and credential management
- [Models reference](/reference/models.md) — supported models and provider details
- [Troubleshooting guide](/guides/troubleshooting.md) — diagnose common setup issues
