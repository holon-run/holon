# Session Handoff

Date: 2026-04-14
Context: handoff after the April runtime contract, execution-boundary, and
dogfooding loop work

## Current state

`pre-public runtime` is now in a usable internal-preview state.

It is not ready to be described as a strongly sandboxed or fully generalized
multi-backend agent runtime, but it is ready for:

- internal dogfooding
- supervised local use
- using `pre-public runtime` to iterate on `pre-public runtime` itself

The main reason this is now true is that the core runtime contract has stopped
being purely conceptual. The following product surfaces are landed enough to be
used together:

- explicit workspace entry and projection
- closure / continuation / objective state
- host-local execution policy phase 1
- instruction loading
- minimal TUI-based foreground interaction

## What has landed

### 1. Workspace and execution model

The `pre-public runtime` runtime now has a stable host-local workspace model built around:

- explicit workspace entry
- `projection_kind = canonical_root | git_worktree_root`
- `access_mode = shared_read | exclusive_write`
- occupancy / lifecycle handling around workspace transitions

This is enough to support real coding loops without continuing to rely on
implicit daemon cwd semantics.

### 2. Runtime semantics

The runtime contract around long-lived agents has materially improved:

- result closure
- continuation trigger resolution
- objective / delta / acceptance boundary state

These are now runtime-visible and tested rather than remaining RFC-only ideas.

### 3. Instruction loading

Instruction loading is now good enough for real use:

- `agent_home/AGENTS.md`
- `workspace_anchor/AGENTS.md`
- `CLAUDE.md` fallback where applicable

This means the agent guidance surface is no longer the main blocker for
dogfooding.

### 4. Minimal TUI

A minimal runtime TUI is now landed.

This is important because the previous main usability blocker was not runtime
capability but lack of a practical foreground interaction loop. The TUI gives
`pre-public runtime` a usable operator-facing surface for:

- picking agents
- inspecting briefs / transcript / tasks
- sending prompts
- observing workspace and runtime state

### 5. Local control client stability

The local Unix-socket control client was fixed so the TUI and other local
control paths no longer prematurely close request writes.

This removes a real reliability problem from the local interaction path.

## Important conclusions from this session

### 1. `pre-public runtime` can now be evaluated as a product, not just as an RFC bundle

The right next step is not more broad RFC expansion. The right next step is to
use the product more aggressively and see where the real operator loop breaks.

### 2. The current execution boundary is still host-local and weakly sandboxed

This is an acceptable preview-stage reality, but it must remain explicit.

Do not describe the current system as if it already provides:

- strong sandbox isolation
- precise resource mediation
- backend-neutral execution

Those directions are documented, but they are not the present product.

### 3. Future backend-mediated execution should remain a direction, not a
phase-1 implementation target

Container and remote backends are legitimate future directions, but the current
codebase should not be prematurely abstracted around them.

The current rule should remain:

- avoid locking public contracts to host-local assumptions where unnecessary
- do not over-rotate into multi-backend architecture before dogfooding pressure
  demands it

## What is not yet done

The main unfinished areas are not hidden. They are simply not blocking an
internal preview:

- stronger execution policy beyond host-local phase 1
- true virtual execution environments / stronger sandboxing
- hierarchical `AGENTS.md` loading
- remote or container-backed execution

These remain valid future work, but they should not be treated as blockers for
starting a real dogfooding phase.

## Recommended next topics for the next session

Pick one main thread, not several:

1. Dogfood the new TUI on real `pre-public runtime` maintenance tasks
   - use `pre-public runtime` to inspect issues
   - enter workspaces
   - make and verify small changes
   - identify where the operator loop is still awkward

2. Tighten preview packaging and operator docs
   - startup path
   - provider config
   - workspace entry flow
   - TUI usage
   - explicit statement of current execution-boundary limitations

3. Resume feature work only after dogfooding reveals a concrete pain point
   - avoid expanding abstractions by default
   - prefer product pressure over speculative cleanup

## Working rule for the next agent

Treat `pre-public runtime` as a product that is ready for supervised internal use.

Do not resume from the assumption that more RFC work is the default next move.

Start from:

- real operator interaction
- real workspace entry
- real prompt / task / transcript flow

Then let the next missing product capability reveal itself through use.
