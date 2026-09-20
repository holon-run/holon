---
title: 排查 Holon 任务问题
summary: 把卡住、失败或无输出的任务定位到一个明确的下一步。
order: 30
---

# 排查 Holon 任务问题

任务卡住通常就那么几种。按你遇到的现象往下走，做检查，再进入下一步。运行时字段
和端点见[参考](/zh-CN/reference/)。

## Daemon 问题

### Daemon 启动不了

**症状：** `holon daemon start` 卡住或报错退出。

**排查清单：**

```bash
# 1. 检查是否已有实例在运行
holon daemon status

# 2. 查看最近的 daemon 日志
holon daemon logs

# 3. 重启 daemon
holon daemon restart
```

如果 daemon 绑定端口失败，换一个端口：

```bash
holon daemon start --port 8788
```

### 启动后 daemon 状态显示“已停止”

查看 daemon 日志里的崩溃信息：

```bash
holon daemon logs
```

常见原因：
- 端口已被其他进程占用
- 默认模型缺少凭据或凭据无效
- 磁盘空间不足，或 `~/.holon/` 权限不足

## 凭据与模型问题

### “No available model” 或凭据错误

最快的修复方式是运行交互式引导向导：

```bash
holon onboard
```

运行诊断工具：

```bash
holon config doctor
```

它会显示哪些模型可用、配置了哪些凭据，以及每个提供商/模型组合的详细可用状态。

### API key 未被识别

1. 确认凭据已保存：
   ```bash
   holon config credentials list
   ```

2. 环境变量鉴权时，检查变量是否已设置：
   ```bash
   echo $DEEPSEEK_API_KEY
   ```

3. 确认提供商已注册，且凭据配置匹配：
   ```bash
   holon config providers list
   ```

4. 使用自定义提供商时，检查 `--credential-profile` 是否与 `config credentials set` 时用的配置一致：
   ```bash
   holon config providers get my-provider
   ```

### 模型报错或行为异常

1. 确认模型可用：
   ```bash
   holon config models list
   ```

2. 检查当前默认模型：
   ```bash
   holon config get model.default
   ```

3. 尝试切换到其他模型：
   ```bash
   holon config set model.default "anthropic@default/claude-sonnet-4-6"
   ```

## Agent 问题

### Agent 不响应

1. 检查 Agent 是否存在：
   ```bash
   holon agent model get my-agent
   ```

2. 用全新的单次运行试试：
   ```bash
   holon run "test" --agent my-agent --max-turns 1
   ```

### Agent 使用了错误的模型

检查 Agent 是否有单独的模型覆盖：

```bash
holon agent model get my-agent
```

清除覆盖，回退到全局默认：

```bash
holon agent model clear my-agent
```

## 配置问题

### 配置改动没有生效

1. 确认键值设置正确：
   ```bash
   holon config get <KEY>
   ```

2. 检查键名有没有拼写错误：
   ```bash
   holon config schema
   ```

3. 如果是直接编辑 `~/.holon/config.json` 做的改动，验证 JSON：
   ```bash
   holon config list
   ```

4. 配置改动后重启 daemon：
   ```bash
   holon daemon restart
   ```

### 配置被重置为默认值

检查是否设置了 `HOLON_HOME`，它会改变配置文件位置：

```bash
echo $HOLON_HOME
```

## TUI 问题

### TUI 显示错乱

尝试禁用备用屏幕：

```bash
holon tui --no-alt-screen
```

或在配置中设置：

```bash
holon config set tui.alternate_screen never
```

### TUI 无法连接 daemon

- 确认 daemon 正在运行：`holon daemon status`
- 检查 daemon 的访问模式：本地 TUI 连接要求 `holon daemon start --access local`
- 远程连接时，确认 `--connect` URL 和 `--token` 正确：
  ```bash
  holon tui --connect https://your-server:8787 --token "your-token"
  ```

## 日志与诊断

### 查看 Agent 任务输出

任务输出存放在 `~/.holon/agents/<agent-id>/task-output/` 下。每个任务有一个以任务 ID 命名的 `.log` 文件。

### 完整系统诊断

```bash
holon config doctor
```

它会报告：默认模型、回退模型、各模型可用性及原因、提供商设置、凭据状态和重试策略。

### 查看运行时配置

```bash
# 完整配置转储
holon config list

# 所有可用键，含默认值和说明
holon config schema
```

## 运行时性能诊断

Holon 跨轮次生命周期各阶段、提供商交互、工具执行和持久化跟踪细粒度性能指标。这些指标是累积的，覆盖从 daemon 启动以来的整个进程生命周期。

### 获取指标

```bash
curl http://127.0.0.1:7878/api/control/runtime/performance
```

响应是按类别分组的 JSON 快照：

### 指标分组

| 分组 | 关键指标 | 关注点 |
|-------|-------------|---------------|
| `turn.*` | `turn.total`, `turn.context_build`, `turn.provider_round`, `turn.tool_execution`, `turn.cleanup` | 上下文构建变慢（提示词拼装增长），工具时间占比过高 |
| `provider.*` | `provider.request_build`, `provider.round_total`, `provider.retry` | 重试次数高或延迟指向提供商问题 |
| `tool.execution` | 累积工具耗时 | 找出 `avg_ms` 异常高的工具 |
| `storage.*` | `storage.append_event`, `storage.persist_state` | 持久化变慢说明数据库有压力 |
| `projection.agent_state.*` | `tasks`, `timers`, `work_items`, `waiting_intents`, `external_triggers` | 每个 Agent 的状态投影开销 |
| `projection.agents_list` | Agent 列表投影耗时 | 仪表盘/API 列表延迟 |
| `http.*` | 各路由 HTTP 耗时 | 慢端点、大负载 |
| `scheduler.*` | `scheduler.poll.all`, `.message`, `.idle`, `.stopped` | 调度器健康度（空闲比例、轮询延迟） |

每个指标条目包含：

| 字段 | 说明 |
|-------|-------------|
| `count` | 该阶段的总调用次数 |
| `total_ms` | 累积墙钟时间 |
| `max_ms` | 单次最慢调用 |
| `avg_ms` | 平均调用耗时 |

### 常见诊断模式

**`turn.tool_execution` 偏高** — 某个工具占用了大部分轮次时间。运行 `holon debug latency` 找出是哪个工具。

**`provider.retry` 频繁** — 模型提供商在返回错误或超时。检查提供商日志（`holon daemon logs`）和网络连通性。

**`turn.context_build` 增长** — 提示词拼装变慢，通常说明累积的历史需要压缩。对照配置的压缩阈值检查 Agent 的 `total_message_count`。

**`scheduler.poll.idle` 比例偏高** — 没有 Agent 处于唤醒状态时属正常。如果有 Agent 有待处理工作，检查唤醒提示和等待条件。

### 重置指标

指标在 daemon 重启时重置。想观察某个特定场景时，重启 daemon、复现问题，然后抓取快照：

```bash
holon daemon restart && sleep 2 && curl http://127.0.0.1:7878/api/control/runtime/performance
```

## 另见

- [配置参考](/zh-CN/reference/configuration.md) — 配置键与凭据管理
- [运行你的第一个 Holon 任务](/zh-CN/guides/quick-examples.md) — 常见任务示例
- [快速开始](/zh-CN/getting-started/first-agent.md) — 首次运行流程
