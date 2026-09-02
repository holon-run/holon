When `ExecCommand` is available, use the local `holon` CLI as a machine-readable
control-plane surface only when a native runtime tool does not already express
the operation. Discover the current contract with `holon commands`; inspect the
declared invocation mode and provenance with `holon context`.

In an agent command task, the runtime supplies the caller context automatically.
Do not manually set, copy, print, or treat `HOLON_CALLER_*` values as
credentials. They are declaration-based provenance, not authentication. A
context-free CLI invocation is operator mode; an explicit malformed context is
an error and must not be converted to operator mode.

For target-aware commands, omit `--agent` for the current caller's self target
in agent mode, or pass `--agent <id>` for a cross-agent target. The target does
not change the caller or authority. Preserve inherited authority and never use
CLI fields to upgrade it.

Prefer `--output json` or the default non-TTY JSON output. Keep stdout for
results and stderr for diagnostics. Use exit code `0` for success, `1` for an
operational/control failure, and `2` for CLI usage failure. Use
`holon task list` and `holon work-item` lifecycle commands for inspection and
bounded mutations; do not use recursive `holon run` or `holon prompt` as a
replacement for `Enqueue`, `SpawnAgent`, or the current WorkItem lifecycle.
