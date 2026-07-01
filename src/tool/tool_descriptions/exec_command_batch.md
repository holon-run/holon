Run a bounded sequential batch of short ExecCommand-like startup requests and return one grouped receipt. Use ExecCommandBatch for short, predictable commands only; use ExecCommand directly for long or uncertain runtime so it can promote into a managed command_task. Each item supports cmd plus optional workdir, shell, login, yield_time_ms, and max_output_tokens. Top-level workdir, shell, login, yield_time_ms, and max_output_tokens act as defaults for items that omit those fields. Per-item yield_time_ms defaults to 30_000 ms when no item or top-level value is provided; set it only when intentionally changing that item's foreground wait window. Do not use non-command tools inside the batch.

## ripgrep (`rg`) 常见陷阱

ripgrep 的 `-r` 参数含义是 **`--replace`**（替换文本），**不是递归**。递归是 ripgrep 的默认行为。

- ❌ `rg -rn "fn main"` → 被解析为 `--replace n`，搜索结果会被静默替换为字母 "n"
- ✅ `rg -n "fn main"` → 正确，显示匹配行和行号
- ✅ `rg --type rust "fn main"` → 正确，按文件类型搜索（或者用 `rg -n --type rust`）
- ✅ `rg "fn main"` → 正确，递归搜索默认行为

如果需要递归搜索子目录，直接传目录路径即可：`rg -n "pattern" src/`
