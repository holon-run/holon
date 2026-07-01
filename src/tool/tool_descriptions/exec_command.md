Start a shell command inside the workspace. Valid startup input uses `cmd` plus optional `workdir`, `shell`, `login`, `tty`, `duplicate_policy`, `accepts_input`, `yield_time_ms`, and `max_output_tokens`; do not pass result or task metadata such as `status` or `task_handle`. `duplicate_policy` defaults to `reuse_running` and `yield_time_ms` defaults to 10_000 ms when omitted; set it only when intentionally changing the foreground wait window. Short commands return immediately; long non-interactive commands become command_task automatically.

## ripgrep (`rg`) 常见陷阱

ripgrep 的 `-r` 参数含义是 **`--replace`**（替换文本），**不是递归**。递归是 ripgrep 的默认行为。

- ❌ `rg -rn "fn main"` → 被解析为 `--replace n`，搜索结果会被静默替换为字母 "n"
- ✅ `rg -n "fn main"` → 正确，显示匹配行和行号
- ✅ `rg --type rust "fn main"` → 正确，按文件类型搜索（或者用 `rg -n --type rust`）
- ✅ `rg "fn main"` → 正确，递归搜索默认行为

如果需要递归搜索子目录，直接传目录路径即可：`rg -n "pattern" src/`
