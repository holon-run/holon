# Claude Code vs Codex For Holon

This note explains how `Holon` should interpret the Claude Code versus Codex
comparison from the perspective of the current repository.

The goal is not to decide which upstream system is “better”. The goal is to
decide what `Holon` should continue borrowing, what it should deliberately
avoid, and where it should start shifting from Claude-shaped design toward a
more explicit runtime shape.

## Short Answer

`Holon` should still use Claude Code as the primary design reference for its
core runtime model.

That is the right choice because `Holon` is trying to be:

- headless
- event-driven
- long-lived
- explicit about trust
- explicit about user-facing versus internal output

Those are the areas where Claude Code is currently the stronger conceptual
reference.

But `Holon` should not keep copying Claude Code equally in every layer.

From this point, the recommended direction is:

- keep learning Claude Code for runtime semantics
- start learning Codex for runtime boundaries

In practice:

- Claude should remain the reference for queue, wake/sleep, external ingress,
  provenance, brief output, and background subtask semantics
- Codex should increasingly become the reference for internal modularity,
  session/turn/task separation, tool execution layering, and future multi-entry
  runtime structure

## 1. Why Claude Was The Right Starting Point

The current `Holon` codebase already reflects the right early choice.

Relevant parts of the repository:

- `src/runtime.rs`
- `src/types.rs`
- `src/queue.rs`
- `src/policy.rs`
- `src/brief.rs`
- `docs/runtime-spec.md`
- `docs/claude-code-reference.md`

What `Holon` already borrowed from Claude well:

### Queue-Centered Runtime

`Holon` normalizes operator prompts, timers, webhook events, task results,
internal follow-ups, and control into one queue model.

That is much closer to Claude Code than to a request/response CLI tool.

This is the right foundational choice for `Holon`.

### Explicit Provenance And Trust

`MessageEnvelope` carries:

- `kind`
- `origin`
- `trust`
- `priority`
- `correlation_id`
- `causation_id`

This is one of the strongest Claude-inspired decisions in the repo, and it is
central to what makes `Holon` distinct.

### Brief Output Separate From Internal Execution

`src/brief.rs` and the runtime flow in `src/runtime.rs` preserve a clear split:

- internal queue and tool activity drive work
- `brief` records are the explicit user-facing delivery surface

This is exactly the right direction for a headless long-lived runtime.

### Sleep / Wake / Tick Semantics

`Holon` already treats:

- session sleep state
- timer wakeups
- internal follow-up messages
- `Sleep` as a terminal tool round

as explicit runtime concepts.

That is much closer to the Claude line of thought than to the Codex one, and
for `Holon` this is correct.

### Child Agent As Runtime-Orchestrated Background Work

`child_agent_task` in `src/runtime.rs` is still bounded and simple, but the
basic shape is right:

- subagent work is background work
- it rejoins through `task_status` / `task_result`
- it is not treated as a separate product surface

That is a good Claude-shaped choice for `Holon`.

## 2. Where Holon Is Already Different From Claude In A Good Way

Even though the design reference is Claude Code, `Holon` has already made a few
useful simplifications.

### It Is Headless By Construction

Claude Code is not just a runtime. It is also a product with:

- TUI behavior
- bridge behavior
- channel behavior
- mode overlays
- enterprise and product gating

`Holon` has deliberately refused all of that so far.

That is good. It keeps the runtime legible.

### It Keeps Compaction Deterministic

`src/context.rs` currently uses deterministic local compaction rather than a
model-generated compact pipeline.

That is a good tradeoff for this stage.

Claude Code’s compact system is more advanced, but also far more complex.
`Holon` should not chase that complexity yet.

### It Keeps The Runtime Surface Small

`Holon` currently lives in one crate with a small set of modules. That still
fits the project’s phase.

Codex-style platformization would be premature if it exploded the codebase
before the runtime contract is stable.

## 3. Where Continuing To Copy Claude Too Literally Will Become A Mistake

This is the main architectural warning.

Claude Code is the better reference for:

- behavior
- runtime semantics
- trust thinking
- long-lived agent interaction patterns

It is not automatically the better reference for:

- codebase shape
- module boundaries
- internal orchestration layering

If `Holon` keeps following Claude too literally from here, the likely failure
mode is:

- runtime semantics stay strong
- but internal boundaries get muddy
- `runtime.rs` becomes the place where queue, prompt building, provider loop,
  tool execution, task orchestration, and policy all keep growing together

That would be bad for `Holon`, because the repo guidelines explicitly prefer:

- small
- explicit
- easy to reason about

In other words:

- Claude is the right semantic teacher
- but not always the right structural teacher

## 4. Where Holon Should Start Learning From Codex

This is the part that matters most now.

The current `Holon` implementation is at the point where Codex becomes a useful
second reference.

Not because `Holon` should become Codex-like as a product, but because Codex is
better at explicit runtime boundaries.

### A. Split Session, Turn, And Task More Clearly

Right now `RuntimeHandle` and `runtime.rs` still carry a lot of responsibilities
at once:

- session state
- queue pop / wake / sleep
- prompt assembly
- provider turn call
- tool loop
- task orchestration
- brief persistence

That was acceptable early on, but it should not stay this way.

Codex’s best lesson here is:

- separate session lifecycle from one turn of model execution
- separate workflow kinds from the session actor

For `Holon`, that likely means eventually moving toward boundaries like:

- `session`
- `turn`
- `tasks`
- `queue`
- `policy`
- `brief`
- `provider`

This is already consistent with your `AGENTS.md`.

### B. Split Tool Exposure, Tool Routing, And Tool Execution

Right now `src/tools.rs` still combines several concerns:

- tool schema exposure
- trust-based tool visibility
- tool dispatch
- tool execution
- background command process tracking

This will get harder to reason about as tools grow.

Codex’s useful lesson is not “copy ToolRouter exactly”.
The useful lesson is:

- separate what tools are visible
- from how tool calls are parsed
- from how tool calls are executed
- from how approval/policy is applied

For `Holon`, a future shape could be:

- `tools/spec.rs`
- `tools/registry.rs`
- `tools/dispatch.rs`
- `tools/exec.rs`
- `tools/fs.rs`
- `tools/shell.rs`

That would keep the headless Claude-style runtime semantics, while borrowing a
Codex-style clarity of layering.

### C. Make Prompt Assembly A Distinct Runtime Layer

`src/context.rs` currently builds one rendered prompt string.

That is still fine for the current Anthropic-compatible loop, but it is too
coarse as the runtime grows.

Codex’s useful lesson here is not its exact prompt format. It is the layering:

- stable instructions
- developer/runtime constraints
- contextual user/environment state

`Holon` does not need Claude’s full prompt section system yet.
But it should probably stop thinking in terms of one flat prompt blob.

A better next step would be to split the current prompt builder into explicit
layers such as:

- runtime base instructions
- policy/runtime instructions
- context projection
- current input

That gives you a cleaner path later whether you stay Anthropic-first or not.

### D. Prepare For Multi-Session Without Productizing Too Early

`src/host.rs` already points in a Codex-like direction:

- runtime host
- lazy per-session creation
- one runtime loop per session

This is good.

The next step should not be:

- app server
- desktop integration
- plugin marketplace

The next step should be:

- keep the host/session boundary clean
- make session isolation obvious
- avoid letting transport adapters shape core runtime behavior

This is much closer to the Codex discipline than to Claude’s product-shaped
growth path.

## 5. What Holon Should Still Keep Borrowing From Claude

Even after bringing in more Codex-style boundaries, `Holon` should keep the
Claude direction in these areas.

### External Ingress Model

Claude remains the better reference for:

- remote ingress as first-class queued work
- channels as untrusted external input
- wakeups driven by timers, external events, and system ticks

This is directly aligned with `Holon`’s product intent.

### Trust Boundary Semantics

Claude’s strongest architectural contribution to `Holon` is not tools. It is
the insistence that:

- not all messages are your user
- provenance survives normalization
- external events may influence planning without silently inheriting authority

That should remain non-negotiable in `Holon`.

### Brief / Delivery Model

Claude’s distinction between:

- internal execution
- formal user-facing delivery

is still exactly right for `Holon`.

Codex currently helps less here than Claude does.

### Proactive Runtime Concepts

Even if `Holon` never clones `KAIROS`, the Claude direction around:

- explicit tick
- explicit sleep
- long-lived wake conditions

is still one of the best references for what `Holon` is trying to become.

## 6. What Holon Should Not Copy From Either Side Yet

There are also things `Holon` should deliberately avoid.

### Do Not Copy From Claude Yet

- TUI-driven architectural assumptions
- prompt topology complexity
- full proactive product surface
- bridge/channel product breadth
- enterprise gating and feature flags
- multi-stage compact pipeline

### Do Not Copy From Codex Yet

- workspace-scale crate explosion
- app-server surface
- heavy protocol layering
- plugin and marketplace system
- guardian-style automated approval reviewer
- large persistent state DB architecture

For `Holon` today, both would be premature.

## 7. Concrete Recommendation For The Next Phase

If I translate this into a direct instruction for the next phase of `Holon`,
the recommendation is:

### Keep These Claude-Shaped Decisions

- queue-centered event normalization
- origin/trust-first message envelope
- explicit sleep/wake/task semantics
- brief as a distinct user-facing channel
- subagent/background work rejoining via the same queue

### Start Introducing These Codex-Shaped Boundaries

- split session lifecycle from one turn execution
- split task workflow handling from the main runtime actor
- split tool spec, routing, execution, and policy
- split prompt assembly into explicit layers instead of one rendered string
- keep multi-session host logic cleaner than the single-session loop

### Do This Without Changing The Product Thesis

The product thesis should stay:

- headless
- event-driven
- long-lived
- trust-explicit
- brief-explicit

What should change is the internal shape, not the project identity.

## 8. My Final Judgment For Holon

From the perspective of `Holon`, the right answer is not:

- “switch from Claude to Codex”

The right answer is:

- “keep Claude as the behavioral reference, and use Codex as the structural reference where the codebase is starting to get crowded”

If I had to compress that further:

- `Holon` should remain Claude-like in runtime semantics
- `Holon` should become slightly more Codex-like in internal boundaries

That is the combination most aligned with the current repository goals.
