---
title: "Configure and Switch Models: From Local Ollama to Multi-Cloud Failover"
summary: Run agents offline with zero-token-cost local Ollama models, connect cloud providers via interactive onboarding, inspect active models conversationally, and configure resilient multi-cloud failover chains.
order: 130
---

# Configure and Switch Models: From Local Ollama to Multi-Cloud Failover

Whether you want to run models completely offline without API keys using local Ollama (ensuring absolute data privacy and zero token expense), connect top-tier cloud providers (OpenAI, Anthropic, DeepSeek), or set up resilient failover chains that keep agents working through upstream rate limits and outages, Holon's routing engine handles model management out of the box.

Holon uses a uniform `provider@endpoint/model` route format (such as `ollama@default/qwen2.5-coder:latest` or `deepseek@default/deepseek-chat`). The runtime strictly distinguishes the requested model from the active model in every turn, giving you reliable transparency in chat.

## Prerequisites

- Access to a running Holon daemon (local terminal or [Web GUI](./use-web-gui.md)).
- **For local inference**: [Ollama](https://ollama.com/) installed and running locally (defaulting to `http://localhost:11434`).
- **For cloud providers**: An active provider account and API key (DeepSeek, OpenAI, Anthropic, etc.).
- Credential hygiene: Never paste API keys into public chat prompts, issues, or screenshots. Use the onboarding wizard or environment variables.

## Steps

<a id="local-or-custom-endpoints"></a>
### Scenario 1: Zero-Config Local Inference with Ollama (Offline & Free)

To run agents entirely free and offline, local Ollama is the cleanest starting point:

1. **Pull and serve a model**:
   Download a programming or reasoning model in your terminal:
   ```bash
   ollama pull qwen2.5-coder:latest
   ```
2. **Set as the global default**:
   Holon automatically scans running local Ollama endpoints without requiring API keys:
   ```bash
   holon config set model.default "ollama@default/qwen2.5-coder:latest"
   ```
3. **Verify connectivity**:
   Run the diagnostics command to confirm endpoint health:
   ```bash
   holon config doctor
   ```

### Scenario 2: Connect Cloud Providers via Onboarding

When you need advanced cloud reasoning models, use Holon's interactive onboarding wizard:

1. **Run the onboarding wizard**:
   ```bash
   holon onboard
   ```
   Select your provider (e.g., DeepSeek, Anthropic) from the interactive menu and paste your API key when prompted. Holon automatically creates the credential profile and links the endpoint.
2. **Or inject via environment variables**:
   Export standard keys in the environment where the Holon daemon starts:
   ```bash
   export DEEPSEEK_API_KEY="sk-..."
   export ANTHROPIC_API_KEY="sk-..."
   ```
   *Note: Environment variables must be set in the shell session that launches the Holon daemon. Exporting variables in a temporary client terminal does not alter an already running daemon.*
3. **List available model routes**:
   ```bash
   holon config models list
   ```
   Copy your desired canonical route and set it as default:
   ```bash
   holon config set model.default "deepseek@default/deepseek-chat"
   ```

#### GitHub Copilot (experimental)

Register your own GitHub OAuth app and enable its device flow. Configure its
**client ID**, not its app ID or a first-party editor's client ID:

```bash
holon config set providers.github-copilot.auth.client_id "<your-client-id>"
holon onboard
```

Select `github-copilot` and complete the OAuth device login. Alternatively,
set `GITHUB_COPILOT_CLIENT_ID` in the login process's environment. Holon stores
the resulting credential and client ID in its own `github-copilot` profile;
it does not read IDE credentials or require a client secret.

This direct provider is experimental: successful GitHub login does not
confirm Copilot entitlement, app/integration admission, or model access.
Discover the models available to your account and select a returned
`github-copilot@default/<model-id>` route; there is no static model list.
Holon sends `Copilot-Integration-Id: holon`, never an editor impersonation.
GitHub controls the accepted identities, models, policies, and usage limits.
Removing a local credential does not revoke its authorization at GitHub;
revoke the app in GitHub settings when access should end.

### Scenario 3: Verify Active Models in Chat & Override Per-Agent

In multi-agent systems, you may want your primary agent running on a cost-effective daily model while assigning a heavier reasoning model to a specialized code reviewer.

#### 1. Set a Per-Agent Override

Use `holon agent model set` to assign an agent-specific route:

```bash
# Assign a high-end reasoning model to reviewer
holon agent model set "anthropic@default/claude-sonnet-4-6" reviewer

# Inspect the agent's effective model setting
holon agent model get reviewer

# Clear the override to inherit the global default
holon agent model clear reviewer
```

#### 2. Verify the Active Model in Chat

Do not rely on the LLM's self-reported identity in conversational prose. Ask the agent directly in Web GUI chat:

> "Report your current active model, provider, and fallback status."

The agent inspects authoritative runtime execution metadata:
- **`model.requested_model`**: The preferred route you configured.
- **`model.active_model`**: The exact route that served the current turn.
- **`model.fallback_active`**: Indicates whether an upstream failure caused the turn to fail over to a backup provider.

### Scenario 4: Configure Multi-Cloud Failover

Upstream outages, network jitter, and HTTP 429 rate limits are common in production. Holon provides resilient multi-cloud failover:

1. **Automatic Failover (Recommended)**:
   ```bash
   holon config set model.fallbacks '["auto"]'
   ```
   When the primary route fails, Holon automatically picks the best available route from your authenticated backup providers, preventing interrupted long-lived tasks.
2. **Explicit Ordered Fallback Chain**:
   To control exact failover sequence (e.g., try DeepSeek first, fall back to Claude, and backstop on local Ollama):
   ```bash
   holon config set model.fallbacks '["anthropic@default/claude-sonnet-4-6", "ollama@default/qwen2.5-coder:latest"]'
   ```
3. **Strict Deterministic Mode (Disable Fallback)**:
   For reproducible benchmarks or compliance environments where data cannot leave a designated provider:
   ```bash
   holon config set runtime.disable_provider_fallback true
   ```
   Set it back to `false` when you want failover protection restored.

### Scenario 5: Configure Dedicated Vision and Image Routes (Optional)

Text conversation routes are decoupled from image capabilities. You can configure dedicated vision and image generation routes:

```bash
# Set default image inspection model
holon config set vision.default "anthropic@default/claude-sonnet-4-6"

# Set default image generation model
holon config set image_generation.default "openai@default/dall-e-3"
```

These settings do not affect your chat model. For details, see [Inspect Images](./inspect-image.md) and [Generate Images](./generate-image.md).

## Troubleshooting

- **Local Ollama Connection Refused**:
  Verify the Ollama daemon is running with `curl http://localhost:11434/api/tags`. If Holon runs inside a container, use `http://host.docker.internal:11434`.
- **Environment Variables Ignored**:
  Ensure keys were exported before the Holon daemon process started. Run `holon config doctor` to verify provider credential status.
- **Unexpected Provider in Turn Output**:
  Check if `fallback_active: true` is reported. This indicates your primary route timed out or returned a rate limit, triggering the failover chain. If you require single-provider execution, enable `runtime.disable_provider_fallback true`.

---

## See Also

- [Models, Providers, and Routing Concepts](/concepts/models-and-providers.md)
- [Use the Web GUI](./use-web-gui.md)
- [Configure the Decision Subsystem](./configure-decision-subsystem.md)
- [Run Long-Lived Tasks and Recovery](./run-long-lived-task.md)
