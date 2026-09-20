---
title: CLI 退出码
summary: Holon 命令行界面的退出码与流路由契约。
order: 13
---
<!-- maintenance: hand-written; keep in sync with the top-level error renderer in `src/main.rs`. Last reviewed against v0.44.1. -->

# CLI 退出码

本页定义 `holon` 命令当前的退出码契约。它面向脚本编写，是
[CLI 稳定性策略](/zh-CN/reference/cli-stability-policy.md)和
[CLI 契约清单](/zh-CN/reference/cli-contract-inventory.md)的配套页面。

Holon 在运行时进入 1.0 之前，有意把这份契约保持得尽量小：

| 退出码 | 含义 | 流契约 |
|---:|---|---|
| `0` | CLI 命令成功完成。 | 机器可读命令把 JSON 或文档化的原始响应写到 stdout。人类可读命令可以把文本写到 stdout。stderr 始终是诊断/日志输出。 |
| `1` | 解析器接受了命令，但在产出成功的 CLI 结果之前执行失败。包括无法连通的控制平面请求、无效的运行时/配置/提供商设置、文件 IO 失败、存储配置格式错误，以及客户端暴露的 HTTP/控制平面错误。 | stdout 不保证脚本安全，通常应为空。诊断由顶层错误渲染器写到 stderr，可能包含链式上下文。 |
| `2` | 在命令执行前，Clap 拒绝了这次调用，例如未知参数、缺少必需参数、枚举值无效或数值范围无效。 | stdout 为空。Clap 把用法/错误文本写到 stderr。 |

本表之外的退出码不属于 Holon 稳定的 CLI 契约。特别是 POSIX 由信号派生的退出
状态由操作系统负责，不应解读为 Holon 的业务结果。

## 典型场景

### 参数无效

解析失败以退出码 `2` 退出：

```bash
holon --definitely-not-a-holon-flag
echo $? # 2
```

这类退出码只用于调用形态问题。脚本应把 stderr 当作人类可读的帮助/错误文本，
而不是机器可读的 schema。

### 控制平面不可达

需要本地或远程控制平面的命令，在传输失败时以退出码 `1` 退出：

```bash
HOLON_HTTP_ADDR=127.0.0.1:9 holon agent status
echo $? # 1
```

对传输失败，CLI 不会在 stdout 上合成 JSON 错误信封。调用脚本应使用 stderr
做操作者诊断，并据此决定重试或退避。

### 配置或提供商设置无效

能通过 CLI 调用解析、但被 Holon 配置校验器拒绝的配置，以退出码 `1` 退出：

```bash
holon config providers set script-test \
  --transport openai_responses \
  --base-url not-a-url
echo $? # 1
```

启动时校验存储配置的命令，对格式错误或不支持的配置也使用退出码 `1`。

### 传输成功但业务状态失败

当命令成功与控制平面通信时，Holon 的 CLI 退出码反映的是命令的传输/结果渲染
结果，而非返回 JSON 中嵌入的每个业务状态。例如，一个成功创建、查看或返回任务
记录的命令，只要 HTTP 请求和响应渲染成功就以 `0` 退出，即使返回的任务或运行时
对象处于 `failed`、`cancelled`、`blocked` 或 `waiting` 等业务状态。

脚本必须检查已文档化的 JSON 字段来获取业务状态。只有当某命令的专属参考页记录
了相应行为时，Holon 才会把业务状态提升为非零进程退出码。

## stdout 与 stderr

- 只有当命令的参考页或清单条目说明它会输出 JSON 或文档化的原始响应体时，才把
  stdout 视为机器可读。
- 把 stderr 视为人类诊断。它可能包含 Clap 用法文本、anyhow 上下文链、tracing
  日志、提供商诊断或操作系统错误。
- 不要为稳定自动化解析 stderr 的具体文本。需要稳定的机器契约时，优先使用 JSON
  输出或 HTTP/API 错误信封。
