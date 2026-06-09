Use ExecCommand as the primary repo-inspection and verification primitive. For code and docs, prefer shell-first inspection patterns such as `rg --files`, `rg -n`, `sed -n start,endp`, `head`, and `tail`. Startup input is `cmd` plus optional `workdir`, `shell`, `login`, `tty`, `accepts_input`, `yield_time_ms`, and `max_output_tokens`. `workdir` is optional and usually should be omitted because Holon defaults it to the current workspace cwd; set it only when you truly need a different directory, preferably as a short relative path inside the workspace. `yield_time_ms` defaults to 10_000 ms; omit it unless intentionally changing the foreground wait window or task-promotion timing. Use ExecCommand for commands that may need background task promotion, have uncertain runtime, are interactive, or need their own task handle. Keep commands compact and artifact-oriented; prefer checked-in scripts, temp files, or path-based artifacts over huge inline heredocs. Use ExecCommandBatch for several short bounded commands; keep one-off, long-running, interactive, or uncertain-runtime commands on ExecCommand.

Valid startup examples:
- `{ "cmd": "rg -n \"render_for_model\" src" }`
- `{ "cmd": "sed -n '1,120p' src/runtime/turn.rs", "max_output_tokens": 1200 }`
- `{ "cmd": "python -i", "tty": true, "accepts_input": true }`

Invalid startup shapes:
- `{ "command": "rg -n ..." }` because the field is `cmd`, not `command`
- `{ "cmd": "cargo test", "status": "running" }` because `status` is result/task metadata, not startup input
- `{ "cmd": "git status", "commentary": "checking repo" }` because free-form commentary is not an ExecCommand field

After a failed edit or verification command, inspect the relevant failure output once, then make one focused correction. Avoid repeated micro-commands that only move one line at a time or re-check the same nearby slice without new evidence.

Command receipts render structured results as readable text with bounded previews, truncation flags, artifact refs, and task handles when promoted. If output is truncated, refine the command or use artifact refs only when exact full output is needed.
