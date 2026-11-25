# Daemon CLI JSON Envelope

Decision:

- `holon daemon --json <command>` is the only machine-readable daemon CLI
  surface. One `schema_version/command/status/data/error` envelope is written
  to stdout per invocation, including parse/usage failures.
- The same outcome drives both renderers: `ok -> 0`, usage/internal -> `1`,
  expected domain failure -> `20`, blocked precondition or daemon-unavailable
  -> `21`, and `22` is reserved for `daemon validate-startup` reporting
  `migration_required`.
- Human output is the default and is bounded: `status: ...`, a few
  command-relevant fields, and one next action on failure.
- `daemon status` keeps its lifecycle view in `data` even when the outcome is
  `blocked` (exit `21`), so GUI consumers can reconcile state while scripts
  still see the blocked exit code.
- There is no compatibility layer for the previous unwrapped daemon JSON.

Reason:

- Before this change every daemon subcommand serialized its own result and
  left failures on the top-level human `Error:` path, so scripts depended on
  command-private success shapes and could not distinguish usage, domain, and
  blocked failures.
- One envelope lets the lifecycle commands and the upcoming
  `daemon validate-startup` (offline startup validation) share error
  classification, exit codes, redaction, and fixtures instead of growing
  separate contracts.
- Parse failures are part of the contract, so `argv` is pre-scanned for a
  daemon invocation before clap reports an error. The normalized `command`
  path comes from that scan and is therefore available even when the typed
  command tree never materialized.

Boundaries:

- No daemon HTTP/API wire contract and no runtime lifecycle semantics change;
  only the CLI output mode and error surface are unified.
- Error text is curated for classified failures. Unclassified failures keep a
  sanitized one-line summary and are reported as `daemon_internal_error`, with
  raw details left to stderr diagnostics.
- Relevant absolute filesystem paths stay inside command payloads; human and
  error text redacts URL userinfo/query and secret-shaped flag values.
- `daemon validate-startup` keeps its approved phases, failure codes, and
  `20/21/22` meanings as the envelope `data` payload; `daemon_cli` reserves the
  `daemon.validate-startup` command path and exit code `22` but does not
  implement those startup-phase semantics.

Integration point:

The shared renderer and exit-code mapping live in `src/daemon_cli.rs`, and
`daemon_command_path` in `src/main.rs` is the single place that maps a
`DaemonCommands` variant to its dot path. Adding `validate-startup` means
adding that variant plus a `DaemonCommandPayload::Data`/`BlockedWithData` result; it
must not add a second JSON shape.
