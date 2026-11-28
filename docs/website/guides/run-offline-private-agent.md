---
title: "Run Private Agents in Offline and Self-Hosted Environments: From Ollama to Private Search and Local Toolchains"
summary: "Deploy Holon in air-gapped or self-hosted intranet environments: lock local Ollama/vLLM models, disable cloud fallback, configure SearXNG or command-based local search, enforce WebFetch host allowlists, set local toolchain rules, and verify boundaries in chat."
order: 135
---

# Run Private Agents in Offline and Self-Hosted Environments: From Ollama to Private Search and Local Toolchains

This guide shows how to run Holon without public cloud LLMs or external SaaS dependencies. It covers two common isolation setups:

- **Air-gapped offline environments**: The host has no internet access or forbids all external network traffic. Inference, tool execution, workspace edits, and search stay completely local.
- **Self-hosted intranet environments**: Agents may reach approved internal services (such as self-hosted Ollama/vLLM, SearXNG, GitLab CE, or GlitchTip), while blocking public cloud model fallback and unlisted internet hosts.

You first configure the daemon's baseline model lock and network policy on the host, then drive repository attachment, `AGENTS.md` boundaries, and end-to-end verification in natural language through the [Web GUI](./use-web-gui.md).

## Prerequisites

- A working Holon installation with permission to edit `<HOLON_HOME>/config.json` and restart the daemon.
- **A running local model server**:
  - Local or intranet [Ollama](https://ollama.com/) (default `http://127.0.0.1:11434`) or an OpenAI Chat Completions-compatible [vLLM](https://docs.vllm.ai/) server (default `http://127.0.0.1:8000/v1`), with your target weights already downloaded.
  - If you run multi-turn coding agents on Apple Silicon or a 64GB unified-memory workstation, see [Can Local LLMs Complete Multi-Turn Coding Agent Tasks? Benchmarking GGUF, MoE Turn Bloat, and Prompt Caching](../blog/local-model-agent-benchmark.md) for measured recommendations (for example, `qwen3.8:27b-gguf` or `gemma4:26b`, and writing a `Modelfile` on disk instead of piping `ollama create -f -`).
- **Optional intranet services (self-hosted mode only)**: Docker host for SearXNG, an internal Git server, or a local documentation search script.

---

## Step 1: Lock Local Models and Disable Cloud Fallback

Setting `model.default` to a local model is not enough on its own. If cloud API keys exist in the daemon's environment and provider fallback remains enabled, a local timeout or error can fail over to a cloud provider.

Before starting or restarting the Holon daemon, lock the default model and disable fallback on the host:

### Option A: Local Ollama (No API Key Required)

```bash
holon config set model.default "ollama@default/qwen3.8:27b-gguf"
holon config set runtime.disable_provider_fallback true
```

If you pull a GGUF tag directly from the Ollama library (avoiding default MLX variants on Apple Silicon), you can also use routes such as `ollama@default/qwen3.8:27b-q4_K_M` or `ollama@default/gemma4:26b`.

### Option B: Local or Intranet vLLM (No API Key Required)

```bash
# When vLLM serves at default http://127.0.0.1:8000/v1
holon config set model.default "vllm@default/<your-served-model>"
holon config set runtime.disable_provider_fallback true
```

If Ollama or vLLM runs on another GPU server on your LAN—or if Holon runs inside a container and reaches the host—update the provider's `base_url`:

```bash
# Example: point Ollama to an intranet GPU server
holon config providers set ollama --transport anthropic_messages --base-url "http://10.0.10.20:11434" --credential-source none --credential-kind none
```

> **Tip**: Leave cloud `vision.default`, `image_generation.default`, and `x_search` unconfigured. To ensure no cloud routes are ever eligible, unset any leftover `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `DEEPSEEK_API_KEY`, or `XAI_API_KEY` variables before launching the daemon.

---

## Step 2: Configure Web and Search Policy for Your Boundary

Holon gates `WebFetch`, `WebSearch`, and `XSearch` at the runtime level. Pick the mode that matches your environment.

### Mode A: Air-Gapped Offline — Disable External Web Tools

In a disconnected environment, turn off public web fetching and search so agents do not waste turns waiting for network timeouts:

```bash
holon config set web.fetch.enabled false
holon config set web.search.enabled false
holon config set x_search.enabled false
```

If you still want agents to use `WebSearch` against a local offline knowledge base (such as local Markdown wikis or offline documentation indexes), keep `web.search.enabled true` and configure a `kind=command` search provider that executes purely on disk without HTTP requests:

```json
{
  "web": {
    "search": {
      "enabled": true,
      "provider": "local_docs",
      "mode": "single"
    },
    "providers": {
      "local_docs": {
        "kind": "command",
        "command": {
          "argv": ["/usr/local/bin/search-local-docs", "--query", "{{query}}", "--limit", "{{max_results}}"]
        },
        "output": {
          "format": "json",
          "mapping": {
            "title": "$.results[*].title",
            "url": "$.results[*].url",
            "snippet": "$.results[*].snippet"
          }
        }
      }
    }
  }
}
```

### Mode B: Self-Hosted Intranet — Private Search and Strict Fetch Allowlist

In a self-hosted intranet setup, you want agents to reach internal services (such as SearXNG, GitLab CE, GlitchTip, or internal docs) while blocking public internet egress:

1. **Connect self-hosted SearXNG for `WebSearch`**:
   Follow [Set Up Self-Hosted SearXNG as Holon WebSearch Provider](./searxng-websearch-provider.md), then make your local instance the default and first automatic candidate:

   ```bash
   holon config set web.providers.searxng.kind searxng
   holon config set web.providers.searxng.base_url "http://127.0.0.1:8888"
   holon config set web.search.enabled true
   holon config set web.search.provider searxng
   holon config set web.search.providers '["searxng"]'
   holon config set web.search.mode single
   holon config set web.search.max_provider_attempts 1
   holon config set web.search.builtin_provider.enabled false
   holon config set x_search.enabled false
   ```

   The attempt cap also applies to explicit aggregation. It is not a provider allowlist: use network egress controls to enforce the intranet boundary.

2. **Restrict `WebFetch` with `web.fetch.allowed_hosts`**:
   By default, `WebFetch` blocks loopback (`127.0.0.1`, `[::1]`) and private network ranges (`10.0.0.0/8`, `192.168.0.0/16`, etc.) to prevent SSRF. Setting a non-empty `web.fetch.allowed_hosts` list serves two purposes at once: **it explicitly permits those listed internal hosts/ports while blocking every unlisted public internet host**:

   ```bash
   holon config set web.fetch.enabled true
   holon config set web.fetch.allowed_hosts '["127.0.0.1:8080", "git.internal.corp", "docs.internal.corp"]'
   ```

   Cloud metadata IPs (such as `169.254.169.254`) remain unconditionally blocked even if added to `allowed_hosts`.

---

## Step 3: Lock Down Decision and Observability Subsystems

1. **Advisory Decision (`AdvisoryDecision`)**:
   Keep `decision.enabled` set to `false` if unused. If you need advisory decisions in an air-gapped environment, use embedded local ONNX (`local-onnx`) pointing at a pre-staged directory on disk rather than downloading presets at runtime:

   ```bash
   holon config unset decision.model
   holon config set decision.local_onnx.model_dir "/opt/holon/models/jev-selector-q4f16"
   holon config set decision.local_onnx.enabled true
   holon config set decision.enabled true
   ```

   See [Configure Advisory Decisions](./configure-decision-subsystem.md) for details.

2. **Observability (OpenTelemetry)**:
   OTLP trace export is disabled by default (`runtime.observability.otlp.enabled = false`). For centralized intranet telemetry, point the endpoint strictly to a localhost or LAN collector (e.g., `http://127.0.0.1:4318/v1/traces`); see [Configure Observability](./configure-observability.md).

Restart the Holon daemon to apply host changes and verify configuration health:

```bash
holon config doctor
```

---

## Step 4: Attach Workspaces and Set Offline Toolchain Rules in Chat

Once the daemon is running, handle repository attachment, isolation boundaries, and standing toolchain rules directly in [Web GUI](./use-web-gui.md) chat.

### 1. Attach a Local Repository and Enforce Worktree Isolation

Tell your agent in chat:

> "Attach `/srv/repos/internal-service` as a workspace. Create a dedicated Git worktree from `main` for all upcoming edits and test runs, and do not modify the main checkout."

The agent calls `AttachWorkspace` and `CreateWorktree` to isolate edits and test commands. See [Work in a Project Workspace or Git Worktree](./work-with-workspaces.md) for safety checks and cleanup behavior.

### 2. Record Standing Offline Toolchain Rules in `AGENTS.md`

To make local-only toolchain rules persistent across every wake and session, instruct the agent to write them into its standing contract (`AGENTS.md`):

> "Record these standing rules in your `AGENTS.md`:
> 1. You are operating in an offline/intranet private environment. Never run `curl`, `git clone`, `npm install`, `pip install`, or `cargo` commands that attempt to reach public internet registries.
> 2. Run builds and tests using pre-cached local dependencies and offline flags (such as `cargo test --offline` or `npm ci --offline`).
> 3. Base every delivery report strictly on local files and observed test exit codes."

The agent persists these constraints in `agent_home/AGENTS.md` (or the repository's `AGENTS.md`), so they load automatically on every future turn.

### 3. Optional: Connect Self-Hosted Team Services

For a full intranet team workflow without public cloud accounts, pair Holon with self-hosted identity and event sources:

- **Self-hosted SSO**: [GitLab CE OIDC](./gitlab-ce-oidc-authentication.md), [Keycloak OIDC](./keycloak-oidc-authentication.md), or [Casdoor OIDC](./casdoor-oidc-authentication.md).
- **Internal error and web-change webhooks**: [Self-Hosted GlitchTip Error Alerts](./glitchtip-error-alerts.md) or [Self-Hosted changedetection.io](./changedetection-webhook-setup.md).

---

## Step 5: Verify Boundaries and Inspect Audit Receipts in Chat

Before handing off production repositories, run one verification prompt in Web GUI chat to check **model routing, network blocking, and local execution**:

> "Run a private-environment self-check without modifying any source files:
> 1. Report your current `model.requested_model`, `model.active_model`, and `model.fallback_active`.
> 2. Try calling `WebFetch` on `https://example.com` and report whether it is blocked and the exact error.
> 3. If an internal allowlisted service or local search is configured, run one allowed check; then run a minimal read-only test command in the attached workspace and report the exit status."

Confirm the returned evidence:

1. **No cloud model egress**: `model.active_model` matches your local `ollama@default/...` or `vllm@default/...` route, and `model.fallback_active` is `false`.
2. **Public internet blocked**:
   - In air-gapped mode, `WebFetch` and `WebSearch` are disabled or rejected.
   - In intranet allowlist mode, `https://example.com` fails with `network_denied` (not in `web.fetch.allowed_hosts`), while explicitly allowlisted internal hosts succeed.
3. **Auditable local receipts**: Inspect the task details and tool execution log in the Web GUI (see [Observe, Track, and Verify Agent Progress](./observe-agent-work.md)) to verify local command exit codes and confirm zero unintended external calls.

---

## Troubleshooting

- **`WebFetch` blocks localhost or LAN hosts with `network_denied`**:
  Loopback and private IPs are blocked by default. Add the exact hostname or `IP:port` (e.g., `"127.0.0.1:8080"` or `"git.internal.corp"`) to `web.fetch.allowed_hosts` and restart the daemon.
- **Holon cannot reach a running local Ollama server**:
  Check `curl http://127.0.0.1:11434/api/tags` on the host. If Holon runs inside a container while Ollama runs on the host, configure Ollama's `base_url` to `http://host.docker.internal:11434` (or the host's LAN IP) and verify Ollama listens on that interface.
- **Multi-turn coding tasks time out on local hardware**:
  On 64GB Apple Silicon running 27B models, confirm Ollama is not running the MLX variant with slow first-turn prefill. Switch to `ggml-org` `Q4_K_M` GGUF (via an explicit `Modelfile` on disk) and keep default FP16 KV caching; see [the multi-turn local benchmark](../blog/local-model-agent-benchmark.md).
- **`AdvisoryDecision` reports missing presets offline**:
  Do not trigger runtime preset downloads without internet access. Copy the verified model directory to disk, set `decision.local_onnx.model_dir`, and unset `decision.model`.

---

## See Also

- [Run an always-on AI assistant on your own computer](./run-always-on-local-assistant.md)
- [Can Local LLMs Complete Multi-Turn Coding Agent Tasks? Benchmarking GGUF, MoE Turn Bloat, and Prompt Caching](../blog/local-model-agent-benchmark.md)
- [Configure and Switch Models: From Local Ollama to Multi-Cloud Failover](./configure-models.md)
- [Set Up Self-Hosted SearXNG as Holon WebSearch Provider](./searxng-websearch-provider.md)
- [Fetch and Search Web Content](./use-web-tools.md)
- [Work in a Project Workspace or Git Worktree](./work-with-workspaces.md)
- [Configuration Reference](/reference/configuration.md)
