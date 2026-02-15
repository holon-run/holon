# Development Notes

This document is a contributor-oriented reference for developing Holon itself (the CLI, runtime, and bundled agents). It complements:
- `AGENTS.md` (source of truth for repo guidelines)
- `CONTRIBUTING.md` (contribution process)
- `docs/` and `rfc/` (design notes and contracts)

## Architecture at a glance
- Runner (Go): prepares inputs/workspace, runs the container, validates artifacts, and publishes results.
- Agent (in container): bridges the Holon contract to a specific engine/runtime (Claude Code by default).

See:
- `docs/holon-architecture.md` (overview)
- `rfc/0002-agent-scheme.md` (agent contract)
- `docs/agent-encapsulation.md` (image composition notes)

## Build and test
Use the commands in `AGENTS.md` (kept up to date).

### Serve TUI/RPC smoke tests
For smoke-level validation of the serve control-plane interaction loop (RPC connect, send turn, stream lifecycle, and input deletion/editing):

```bash
make test-serve-tui-smoke
```

Equivalent direct command:

```bash
go test ./pkg/tui -run '^TestTUISmoke_' -v
```

CI note:
- These tests are regular Go tests under `pkg/tui`, so they run in CI through the existing `go test ./...` jobs.

## Logging
Holon uses structured, leveled logs.

Log levels:
- `debug`: most verbose
- `info`: general info
- `progress`: progress/status updates (default)
- `minimal`: warnings/errors only

Set log level:
- CLI: `--log-level debug|info|progress|minimal`
- Project config: `.holon/config.yaml` (`log_level: "debug"`)

## Common debugging entrypoints
- `holon solve …`: end-to-end GitHub flow (collect context → run agent → publish).
- `holon run …`: lower-level execution entrypoint (spec/goal driven).
- `holon detect image`: explain and debug base image auto-detection.

## Project configuration (`.holon/config.yaml`)
Holon loads project configuration from `.holon/config.yaml` by searching upward from the current directory.

Typical fields:
```yaml
base_image: auto            # or an explicit image like golang:1.24
agent: default              # agent bundle ref (path/URL/alias)
agent_channel: latest       # latest (default), builtin, pinned:<version>
log_level: progress         # debug, info, progress, minimal
assistant_output: none      # none, stream
skills: [./skills/foo]      # optional skill dirs
git:
  author_name: holonbot[bot]
  author_email: 250454749+holonbot[bot]@users.noreply.github.com
```

Precedence is generally: CLI flags > project config > defaults.

## Base image auto-detection
Holon can auto-detect a toolchain base image from workspace files when `--image` is not provided (and auto-detection is enabled).

- CLI flags:
  - `--image` / `-i`: explicit base image (disables auto-detect for this run)
  - `--image-auto-detect`: enable/disable detection
- Debugging:
  - `holon detect image --debug`
  - `holon detect image --json`

Implementation lives under `pkg/image/`.

## Agent config mounting (`--agent-config-mode`)
Holon can optionally mount host agent configuration into the container (currently relevant for Claude agents).

`--agent-config-mode` values:
- `no` (default): never mount (safest; recommended for CI)
- `auto`: mount `~/.claude` if present and compatible
- `yes`: always attempt to mount; warns if missing/incompatible

Security note: mounting host config may expose local credentials/sessions to the container. Avoid enabling this in CI or shared environments.

## Runtime mode (`--runtime-mode`)
Holon supports runtime sourcing modes for the agent code:

- `prod` (default): use the bundled agent code in the composed container image.
- `dev`: overlay bundled agent code with local `dist/` from a source checkout.

Useful flags:
- `--runtime-mode prod|dev`
- `--runtime-dev-agent-source <dir>` (defaults: `HOLON_RUNTIME_DEV_AGENT_SOURCE`, `HOLON_DEV_AGENT_SOURCE`, `./agents/claude`)

When using `--runtime-mode dev`, ensure local agent build artifacts exist:
```bash
cd agents/claude
npm install
npm run build
```

`dev` mode is intended for local debugging iteration. CI should stay on `prod`.

## Preflight checks
Holon runs preflight checks to fail fast when required tooling or credentials are missing.

- `holon run`: checks Docker, git, workspace/output paths by default
- `holon solve`: includes GitHub-token checks (and may run early checks before workspace prep)
- Bypass (not recommended): `--no-preflight` / `--skip-checks`

Implementation lives under `pkg/preflight/`.

## Agent bundle management
Use `holon agent …` to inspect and manage agent bundles/aliases:
- `holon agent install <url> --name <alias>`
- `holon agent list`
- `holon agent remove <alias>`
- `holon agent info default`
- `holon agent init --template <run-default|solve-github|serve-controller> [--force]`

`holon agent init` templates are init-time only. Runtime prompts do not inline persona file bodies from `ROLE.md/AGENT.md/IDENTITY.md/SOUL.md`; the agent reads them from `HOLON_AGENT_HOME`.

Builtin agent resolution notes:
- If no explicit agent is provided, Holon can resolve an agent via `--agent-channel` / `HOLON_AGENT_CHANNEL` (default: `latest`).
- Auto-install can be disabled with `HOLON_NO_AUTO_INSTALL=1` (useful in strict/offline environments).

## Environment variables (high level)
Most end-to-end developer runs need:
- `ANTHROPIC_AUTH_TOKEN` (or equivalent provider token)
- `GITHUB_TOKEN` (or `HOLON_GITHUB_TOKEN`, or `gh auth login`)

Other commonly used variables:
- `HOLON_CACHE_DIR`: overrides the cache directory (default is under `~/.holon/`)
- `HOLON_AGENT`: default agent bundle reference (when not using a channel)
- `HOLON_AGENT_CHANNEL`: agent channel (e.g. `latest`, `builtin`, `pinned:<version>`)

Agent-specific runtime variables (model, timeouts, etc.) are documented in:
- `docs/agent-claude.md`

## Artifacts and contracts
Holon treats an agent run like a batch job with explicit, reviewable outputs. The common artifacts are:
- `diff.patch`
- `summary.md` (optional human-readable output)
- `manifest.json` (required machine-readable execution record)

See:
- `docs/manifest-format.md`
- `docs/workspace-manifest-format.md`

## Publishing
Publishing is handled in skill workflows (for example `ghx`) rather than by a standalone `holon publish` command.

- `holon solve` runs end-to-end in skill-first mode.
- `holon run` executes a spec/goal and emits artifacts (`manifest.json`, `diff.patch`, `summary.md`) for downstream tooling.

## GitHub helper library
Holon centralizes GitHub API behavior in `pkg/github/` so GitHub-facing flows share:
- auth/token handling
- pagination and rate-limit behavior
- typed API helpers (issues, PRs, comments, review threads, diffs, CI)

See: `pkg/github/client.go`, `pkg/github/operations.go`, `pkg/github/types.go`.

## Prompt compiler (assets → system/user prompts)
Holon compiles prompts from composable markdown assets under `pkg/prompt/assets/`:
- Common contract: `pkg/prompt/assets/contracts/common.md`
- Mode overlays: `pkg/prompt/assets/modes/<mode>/contract.md` and `.../context.md`
- Roles: `pkg/prompt/assets/roles/<role>.md` (and optional mode-specific `modes/<mode>/overlays/<role>.md`)

Layer order (bottom → top):
1) common contract
2) role
3) mode contract (optional)
4) mode overlay (optional, `modes/<mode>/overlay.md`)
5) mode context (optional)
6) role overlay (optional)

Defaults are defined in `pkg/prompt/assets/manifest.yaml` (mode + role).

Code:
- compiler: `pkg/prompt/compiler.go`
- embedded assets: `pkg/prompt/assets.go`

When changing prompts, prefer editing the assets under `pkg/prompt/assets/` and validate with existing compiler tests (`pkg/prompt/compiler_test.go`).

## Mock Claude driver (deterministic testing)
The Claude agent supports a mock driver so tests can run without real API credentials:
- enable: `HOLON_CLAUDE_DRIVER=mock`
- fixture: `HOLON_CLAUDE_MOCK_FIXTURE=/path/to/fixture.json`

Implementation lives under `agents/claude/src/mockDriver.ts` and `agents/claude/src/claudeSdk.ts`.
