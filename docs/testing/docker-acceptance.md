# Docker Release Acceptance

This plan defines release-level acceptance testing for the real Holon process.
It complements Rust module and integration tests; it does not replace them.

## Goal

Verify that the built and published Holon artifact works across the process,
HTTP, persistence, workspace, worktree, restart, and upgrade boundaries that
unit tests cannot fully represent.

The primary test subject is:

```text
Docker container -> holon serve -> public CLI/HTTP contracts
```

Tests must not construct `RuntimeHost` directly.

## Environment model

- Run `holon serve` in the foreground as the container's main process.
- Give every scenario an isolated Docker network, `HOLON_HOME`, workspace
  parent directory, and randomly generated control token.
- Do not expose a fixed host port. Use an internal Docker network or a random
  published port.
- Mount the parent of a temporary Git repository so Holon can create sibling
  managed worktrees.
- Use a deterministic scripted provider for default acceptance tests. Keep
  real-provider checks in a separate opt-in live suite.
- Preserve container logs, a bounded runtime-data summary, and workspace or
  worktree metadata when a scenario fails.
- For upgrade tests, start an old release image and the candidate image
  against the same persistent volume in sequence.

The base image contains Holon plus basic shell, Git, SSH, CA, archive, and HTTP
utilities. Project-specific acceptance fixtures may derive from it to install
additional language toolchains.

## Manual real-LLM suite

`make docker-live-acceptance` runs the production image against a real model.
It is intentionally manual: it requires network access, consumes provider
quota, and may expose model nondeterminism. It must not be added to
credential-free CI.

Set the model and its credential environment variable:

```bash
HOLON_LIVE_MODEL='openai/gpt-5.4' \
OPENAI_API_KEY='...' \
make docker-live-acceptance DOCKER_IMAGE=holon:live-acceptance
```

The runner infers the standard credential variable for OpenAI, Anthropic,
DeepSeek, and xAI. For a built-in provider whose variables cannot be inferred,
pass comma-separated variable names:

```bash
HOLON_LIVE_MODEL='openrouter/model' \
HOLON_LIVE_CREDENTIAL_ENVS='OPENROUTER_API_KEY' \
OPENROUTER_API_KEY='...' \
make docker-live-acceptance
```

Alternatively, set `HOLON_LIVE_DOCKER_ENV_FILE` to an untracked Docker env
file. The runner records only that an env file was used, not its path or
contents.

The checked-in case definitions live in
`tests/manual/docker-live-acceptance.json`. The runner is
`scripts/docker-live-acceptance.py`. Run one case with:

```bash
python3 scripts/docker-live-acceptance.py \
  --image holon:live-acceptance \
  --skip-build \
  --case workspace-restart-lifecycle
```

Evidence is written below `target/docker-live-acceptance/<timestamp>/` and
includes prompts, state snapshots, WorkItem read models, event pages,
transcripts, briefs, tool execution details, Git state, and container logs.
Credential values are never written deliberately. Set
`HOLON_LIVE_KEEP=1` only when containers and volumes must be retained for
interactive diagnosis.

### Checked-in cases

#### `workspace-restart-lifecycle`

1. Start `holon serve` in the production image with an isolated `HOLON_HOME`.
2. Attach a mounted Git repository through the authenticated HTTP control
   plane.
3. Ask the real model to inspect workspace state, switch to the repository,
   and create a managed worktree through canonical tools.
4. Restart the real container while retaining the home volume and workspace.
5. Ask the model to recover the registered execution root, switch into it,
   return to the canonical root, and remove it.
6. Assert successful workspace tool events, persisted attachment state, a
   clean canonical repository, and exactly one remaining Git worktree.

#### `workitem-wait-restart-complete`

1. Ask the real model to create and pick one WorkItem with a fixed plan marker
   and todo list.
2. Require `WaitFor(wake=operator_input)` and assert the WorkItem is current,
   open, `needs_input`, and `waiting_for_operator`.
3. Restart the container with the same home volume and assert focus, wait
   state, and plan artifact survived.
4. Ask the model to rediscover the current item through `ListWorkItems` and
   `GetWorkItem`, update its todos, and complete the exact same WorkItem.
5. Assert the required tools succeeded and the durable completion result
   contains the generated completion marker.

These cases validate the complete boundary:

```text
real LLM -> tool selection -> holon serve -> runtime persistence -> HTTP evidence
```

They complement the ignored Rust live tests, which construct `RuntimeHost`
directly and therefore do not cover image packaging, HTTP authentication,
container restart, or persistent Docker volumes.

## Phases

### Phase 0: image smoke

Implemented by `make docker-smoke` and run in CI.

1. Build the production Dockerfile.
2. Run `holon --version` inside the image and compare it with `Cargo.toml`.
3. Start `holon serve` with an isolated named volume and random host port.
4. Poll the authenticated `/api/control/runtime/readiness` endpoint.
5. Remove the container and volume on both success and failure.

This phase is a fast packaging and process-boundary gate, not a full runtime
acceptance suite.

### Phase 1: workspace main paths

The manual real-LLM suite covers the first restart/worktree path. A future
deterministic harness should expand coverage to:

1. Attach and switch a workspace, restart the service, and verify that the
   binding and active projection persist.
2. Spawn isolated child work that modifies files while leaving the canonical
   workspace unchanged.
3. Verify automatic cleanup of clean task-owned worktrees.
4. Verify retention and review metadata for dirty worktrees.
5. Run parallel tasks and multiple agents against conflicting occupancy and
   verify explicit, recoverable outcomes.
6. Read canonical and worktree files through the HTTP workspace API.
7. Reject traversal, symlink escape, stale execution-root generations, and
   unauthorized worktree artifact access.

### Phase 2: recovery and upgrade

Exercise:

1. Force-kill `holon serve`, restart it with the same home, and recover agents,
   WorkItems, tasks, waits, and workspace state.
2. Interrupt tool execution, task completion, and worktree cleanup at
   controlled points; reconciliation must not duplicate side effects.
3. Start from the current recommended release's runtime data and upgrade to
   the candidate image.
4. Cover a locked database, missing workspace path, and orphaned worktree,
   verifying diagnostics and retention behavior.

### Phase 3: published artifact

After a tag release:

1. Pull `ghcr.io/holon-run/holon:<version>` by tag.
2. Run the same image smoke against the pulled digest.
3. Verify the OCI version/revision labels and image tag agree with the GitHub
   release tag.
4. Promote Phase 1 and Phase 2 to release gates after their deterministic
   harness is stable.

## Host-only coverage

Keep a smaller host test lane for behavior hidden or changed by containers:

- `holon daemon start/status/restart/stop`
- Unix socket creation and cleanup
- host file ownership and permission behavior
- installation paths and downloaded tarballs
- platform-specific macOS behavior

Docker acceptance must not be reported as covering these boundaries.

## Pass criteria

- Tests exercise the compiled Holon process through supported CLI and HTTP
  interfaces.
- No default or CI test depends on the operator's local Holon home, a fixed
  port, or real cloud credentials. The opt-in live suite accepts only
  explicitly forwarded credentials.
- Every scenario is repeatable and cleans up resources after success.
- Failures retain enough bounded evidence to identify process logs, runtime
  state, and affected workspace artifacts.
- Phase 0 runs in normal CI. Later phases become release gates only after they
  are deterministic and have an explicit failure-artifact contract.
