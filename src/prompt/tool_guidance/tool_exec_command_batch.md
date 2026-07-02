Use ExecCommandBatch when several short, bounded shell commands should run sequentially before the next decision and do not require interactive input, background task management, or command-task continuation. Each item uses restricted ExecCommand startup fields: `cmd`, optional `workdir`, `shell`, `login`, `yield_time_ms`, and `max_output_tokens`; top-level values act as defaults and item values take precedence. Per-item `yield_time_ms` defaults to 30_000 ms when neither item nor top level sets it; set it only when intentionally changing that item's foreground wait window. ExecCommandBatch does not promote timed-out items into managed command tasks, so use ExecCommand for long-running, uncertain-runtime, interactive, or independently supervised commands. Batch output is one grouped receipt with per-item status, bounded output previews, truncation flags, command previews, and errors. Use it instead of unstructured shell separator scripts when item boundaries matter; keep items compact and artifact-oriented, and use `stop_on_error` when later commands should not run after a failure.

**Important: ripgrep (`rg`) flag usage.** In ripgrep, `-r` means `--replace`, *not* recursive (rg searches recursively by default). Do not use `-rn` as a flag group — it is parsed as `--replace n` (replacing matched text with the letter "n") and does *not* enable line numbers. Use `rg -n` (with line number) or `rg --files` for file listing. When there are no matches, rg exits with code 1 — this is normal behavior, not a command failure.

Valid startup examples:
- `{ "items": [{ "cmd": "git status" }] }`
- `{ "max_output_tokens": 1200, "items": [{ "cmd": "rg -n \"foo\" src" }], "stop_on_error": true }`
- `{ "items": [{ "cmd": "sed -n '1,120p' src/lib.rs" }, { "cmd": "rg -n \"TODO\" src" }], "stop_on_error": true }`

Invalid startup shapes:
- `{ "cmd": "git status" }` because top-level `cmd` is not valid for ExecCommandBatch; if this is a single command, use ExecCommand instead
- `{ "items": [{ "cmd": "python -i", "tty": true }] }` because `tty` and `accepts_input` are ExecCommand-only interactive fields
- `{ "items": [{ "cmd": "cargo check --all-targets" }] }` when the command may run long enough to need promotion; use ExecCommand for long or uncertain runtime
