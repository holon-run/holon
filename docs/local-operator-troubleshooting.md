# Local Operator Troubleshooting

This document defines the recommended local troubleshooting workflow for
`Holon`.

The goal is to give operators one repeatable path instead of guessing between
`run`, `serve`, `daemon`, `status`, and `tui`.

## First Principle

Pick the troubleshooting entry point based on the runtime shape you are using:

- use `holon run` when you want a one-shot reproduction or a single coding /
  analysis turn
- use `holon daemon status` first when you are troubleshooting a long-lived
  local runtime
- use `holon daemon logs` second when `daemon status` or a lifecycle command
  already indicates failure
- use `holon tui` after the runtime is confirmed healthy and you want to watch
  or drive live agent state
- use `holon serve` directly only when you intentionally want the runtime in
  the foreground, such as local development or debugging startup behavior

## Recommended Order Of Inspection

For a long-lived local runtime, inspect in this order:

1. `holon daemon status`
2. `holon daemon logs`
3. `holon status`, `holon tail`, or `holon transcript`
4. `holon tui`
5. `holon daemon restart` or a foreground `holon serve`

This order is deliberate:

- `daemon status` is the cheapest health check
- `daemon logs` is the explicit follow-up for lifecycle or local runtime
  failures
- agent-scoped state comes after runtime health is known
- `tui` is for live observation and interaction, not the first place to debug
  whether the daemon is even healthy

## One-Shot Reproduction

Use `holon run` when you need a fresh, bounded reproduction:

```bash
cargo run -- run "analyze this workspace" --json
cargo run -- run "reproduce the failure" --workspace-root /path/to/repo --json
```

Use it when:

- the failure does not require a long-lived agent
- you want structured output for one run
- you want per-run token usage and provider diagnostics without attaching to an
  existing daemon-managed agent

`holon run --json` is the recommended first step for:

- prompt-level reproduction
- provider failure reproduction
- verifying whether a problem is tied to one agent's persisted state or to the
  model / workspace / tool path itself

When provider diagnostics are available, inspect:

- `token_usage`
- `provider_attempt_timeline`
- the final error summary

These surfaces tell you:

- whether the provider retried
- whether it failed fast on a contract or auth error
- whether fallback advanced to another configured provider

## Long-Lived Runtime Health

Start with:

```bash
cargo run -- daemon status
```

Use `daemon status` to answer:

- is there a healthy runtime for this `HOLON_HOME`
- is it `idle`, `waiting`, or `processing`
- how many public agents and tasks are active
- is there a recent startup, shutdown, or runtime-turn failure summary

Interpretation:

- `idle`: the runtime is healthy and has no visible active or waiting work
- `waiting`: the runtime is healthy but visible work is blocked on a future
  event or task result
- `processing`: at least one public agent is actively running

If `daemon status` already reports a failure summary, go to `daemon logs`
before opening TUI.

## Logs And Failure Details

Use:

```bash
cargo run -- daemon logs
```

`daemon logs` is the stable local inspection surface for:

- `run/daemon.log`
- recent startup failure summary
- recent shutdown failure summary
- persisted runtime metadata and last-failure paths

Use it when:

- `daemon start` or `daemon stop` fails
- `daemon status` reports a recent failure
- you need to inspect daemon-local failure details without guessing filesystem
  paths

## Agent State And Live Observation

Once the runtime is healthy, inspect agent state:

```bash
cargo run -- status
cargo run -- tail --limit 20
cargo run -- transcript --limit 50
```

Use these surfaces to answer:

- which agent is paused, waiting, failed, or completed
- what the last user-facing `brief` was
- what the raw transcript shows for the most recent provider turns
- whether token usage and provider-attempt diagnostics were preserved

Then use:

```bash
cargo run -- tui
```

Use `tui` when:

- you want to watch live state after confirming the runtime is up
- you want to interact with the running agent directly
- you want a chat-first operator surface with transcript / task overlays

Do not use TUI as the first health probe for a suspected daemon lifecycle
failure. Check `daemon status` first.

## When To Use `serve`

Use `holon serve` directly when:

- you are developing the runtime locally
- you want foreground logs immediately in one terminal
- you are debugging startup or control-surface behavior without the daemon
  wrapper

Do not treat `serve` as a different product mode. It is the same long-lived
runtime that `daemon` manages in the background.

## Safe Local Recovery

If the local runtime state seems stale:

1. run `holon daemon status`
2. inspect `holon daemon logs` if a failure is reported
3. try `holon daemon stop`
4. then `holon daemon start`
5. use `holon daemon restart` when you explicitly want a stop/start cycle

Current recovery contract:

- stale local runtime files under `<holon_home>/run/` are recoverable
- `daemon start` cleans stale local state when it is safe to do so
- stale daemon pid files that point to already-exited processes are also
  recoverable; `daemon restart` should clean and continue instead of failing on
  missing-process `kill`
- occupied socket paths owned by unrelated processes fail closed
- `daemon stop` prefers graceful runtime shutdown before fallback cleanup

If you need to debug startup itself, run `holon serve` in the foreground
instead of repeatedly guessing from background behavior alone.

## Recommended Quick Paths

If one prompt failed:

1. run `holon run --json`
2. inspect `token_usage` and `provider_attempt_timeline`

If the long-lived runtime seems unhealthy:

1. run `holon daemon status`
2. run `holon daemon logs`

If the runtime is healthy but work seems stuck:

1. run `holon status`
2. run `holon transcript --limit 50`
3. open `holon tui`

If local lifecycle state looks stale:

1. run `holon daemon stop`
2. run `holon daemon start`
3. if needed, run foreground `holon serve`
