---
title: 排查卡住或无响应的 Agent 任务
summary: 在 Web GUI 对话中优先自检卡住、长时间无输出或失败的 Agent 任务：区分正常的 WaitFor 挂起等待与真实阻塞，逐层核查 TaskStatus 与子 Agent 状态、模型路由、工作树冲突及守护进程故障，并安全恢复推进。
order: 260
---

# 排查卡住或无响应的 Agent 任务

当 Agent 长时间没有回复、后台命令看似停滞、委派出去的子 Agent 未返回结果，或者执行报出模型、凭据与工作区冲突错误时，请按本指南逐层排查。**先只读核查，再决定是否干预**——在搞清楚当前任务处于正常挂起等待还是真实异常之前，不要盲目重发同一条指令、强行重启守护进程或删除 Agent 目录。

## 适用场景

- 对话长时间无新输出，无法判断 Agent 是在执行长命令、等待审批，还是已经卡死。
- 主 Agent 委派子任务后看起来处于空闲状态。
- 执行过程中出现模型提供商报错、凭据失效或 Git worktree 写入冲突。
- Web GUI、TUI 或 CLI 无法连接到本地或远程 Holon 守护进程。

## 前置条件

- Holon 运行时已启动（本地默认 Web GUI 地址 `http://127.0.0.1:7878/`），或具备登录宿主机执行只读 CLI 诊断的权限。
- 明确负责该任务的 Agent ID（例如 `main`、`builder` 或 `reviewer`），并优先围绕现有 `WorkItem` 排查，避免重复派发同一目标造成重复改动或重复评论。

## 排查与恢复步骤

### 1. 在 Web GUI 对话中先自检 WorkItem 与等待状态

在重复下达指令之前，先直接在 [Web GUI](use-web-gui.md) 对话框里让主 Agent 做一次只读自检：

> “检查你当前的开放 WorkItem、调度状态、生效中的 `WaitFor` 条件和活跃任务。你现在是在正常等待依赖，还是遇到了真实阻塞？”

同时可以在 Web GUI 右侧打开 **Work Items** 面板，或将对话流显示级别从 **Info** 切换到 **Verbose / Debug**，查看最近的工具调用与等待标记。对照观察到的状态采取下一步：

| 观察到的状态 | 真实含义 | 下一步动作 |
|---|---|---|
| `runnable`（执行中） | 正在进行模型推理轮次或执行工具调用 | 等待当前轮次完成或询问阶段进展，不要重复发送相同指令 |
| `yielded` | 因切换 WorkItem（如 `PickWorkItem`）而由延续帧暂存挂起 | 查看它让位给哪个活跃 WorkItem；不要手动强行清理无关阻塞 |
| `waiting`（`wake=task_result`） | 零 Token 休眠，等待后台命令或子 Agent 任务完成 | 核查对应任务的 `TaskStatus` 与可用日志，不要重复启动同一命令 |
| `waiting`（`wake=timer` 或 `wake=external`） | 挂起等待定时器触发或外部 Webhook 回调到达 | 核对定时器时间或外部事件源；Webhook 未唤醒参见[外部事件排障](connect-external-events.md#常见问题与排障) |
| `waiting_for_operator`（`needs_input` 或 `wake=operator_input`） | 停在人工决策或授权边界，等待你的输入 | 直接在同一对话中回复确认或补充条件，运行时会自动唤醒原 WorkItem |
| `blocked` | 存在尚未解除的真实阻塞或依赖失败 | 按下文分层定位根因并修复后再解除阻塞 |
| `completed`（或已中断的历史运行） | 该 WorkItem 已结算完成或已停止 | 先查看最终简报与生成产物，确认是否需要新开后续任务 |

### 2. 按分层定位真实阻塞原因

如果确认任务处于 `blocked` 或已报错失败，引导 Agent 逐层收敛排查：

1. **后台命令任务（核对 `TaskStatus` 与可用日志路径）：**

   > “用 `TaskStatus` 检查那个 task ID 的精确状态、退出码，以及是否有可用的 stdout/stderr 日志路径，并读取报错末尾。”

   `TaskStatus` 会返回真实的任务生命周期状态与运行时确认的日志路径。不要自行猜测日志文件名，也不要在未确认退出码或取消原因前把正在运行的命令当作失败。

2. **委派子 Agent（使用轻量 `ListSubagents` 检查）：**

   > “用 `ListSubagents` 列出你直接监督的子 Agent 状态，核对子任务是否仍在运行、等待中还是已经失败。”

   `ListSubagents` 仅做轻量只读监督查询，既不会唤醒未加载的子 Agent，也不会提前消费任务结果。在 Web GUI 的任务详情卡片中，也可以直接点击进入子 Agent 会话视图核对细节。参见[将复杂任务安全委派给子 Agent](delegate-work.md)。

3. **模型路由、凭据与提供商超时：**

   > “核对当前生效的模型路由以及失败轮次的精确报错，不要在原因不明时盲目切换模型。”

   上游瞬时限流或超时与本地缺少凭据是两类问题；未排查清楚前不要随意改写配置。模型配置与故障排查参见[配置与切换大模型](configure-models.md)。

4. **工作区与 Git Worktree 冲突：**

   > “运行 `GetWorkspaceState` 核查当前工作树是否处于脏状态、被锁定或正被其他 Agent 占用。”

   在确认清楚分支与未提交改动归属前，不要强行删除目录或重置工作树。参见[在项目工作区与 Git Worktree 中工作](work-with-workspaces.md)。

### 3. 最小化恢复与续跑

定位到具体原因后，按最小干预原则恢复：

- **等待操作者确认：** 直接在原对话回复决策或授权即可满足 `operator_input`，运行时会自动恢复同一 WorkItem。
- **外部阻塞已修复：** 若你已在外部修复了依赖、补齐凭据或解决了 Git 冲突，但 WorkItem 仍停在 `blocked`，在对话中明确指示 Agent：

  > “确认刚才的阻塞已经解决，用 `PickWorkItem`（设置 `clear_blocker=true` 并注明原因）清除阻塞标记并从下一个待办项继续。”

- **命令或模型调用失败：** 修复配置或脚本报错后，仅让 Agent 重试失败的步骤或验证命令，不要清空代理状态重新来过。
- **重跑前先核实外部副作用：** 如果失败发生在调用 GitHub、推送代码或其他外部写操作之后，先让 Agent 核查外部目标是否已经生效，再决定是否重试，防止重复评论或重复提交。

## 宿主机与 CLI / HTTP 诊断（Web GUI 不可用或无头运维时）

当守护进程无法启动、所有模型路由均不可达，或需要通过 SSH 在无头服务器上排查时，可以在宿主机使用以下只读 CLI 与 HTTP 接口。

### 守护进程启动失败或启动后显示已停止

```bash
holon daemon status
holon daemon logs
holon daemon validate-startup
```

先查看真实启动报错：常见原因包括已有进程占用端口、监听地址冲突、配置文件语法错误、磁盘空间不足，或者当前 shell 与守护进程使用了不同的 `HOLON_HOME`：

```bash
printf '%s\n' "${HOLON_HOME:-<default home>}"
```

不要在不同 `HOLON_HOME` 目录之间直接复制或覆盖运行时数据库。修复根因并重启守护进程后，重新查询未完成的 WorkItem 与任务状态——持久化的 WorkItem 会保留，但运行中的外部命令进程可能已随守护进程停止而中断。

### 从已退役的破坏性操作调度器升级

升级会移除 `ScheduleDestructiveOperation` 及其私有账本。它不会执行已保存的验证命令、重试旧操作，也不会停止已经交给外部执行器的任务。账本里没有记录，并不意味着操作已成功或进程已退出。如果此前派发的任务可能还在运行，请先在外部服务管理器里独立核对它的状态，再决定是否发起同类操作。如需执行已授权的守护进程重启，请从守护进程所在服务之外的操作者会话中操作。

### “No available model” 或凭据失效

```bash
holon config doctor
holon config providers list
holon config model-providers list
holon config models list
holon config credentials list
```

核对提供商配置的 credential profile 是否与凭据库一致。若通过环境变量鉴权，检查变量是否存在（切勿直接打印密钥值）：

```bash
test -n "${DEEPSEEK_API_KEY:-}" && echo "set" || echo "unset"
```

新终端里导出的环境变量不会自动注入已经运行中的守护进程；请使用受管的凭据存储或按文档要求重启守护进程。

若免凭据或匿名访问的本地服务商（`ollama`、`vllm`、`magpie`）未显示模型，请在 `config.json` 中声明 `providers.<id>`、在模型路由中引用它，或运行 `holon config models refresh --provider <id>`。

### 配置修改未生效

```bash
holon config get CONFIG_KEY
holon config schema
```

确认配置键名与作用域（是否被单个 Agent 覆盖）。向他人反馈问题时，请剔除完整 `holon config list` 输出、密钥 Token、回调 Capability URL 与完整对话记录。

### CLI 只读状态与日志检查

```bash
holon agent status builder
holon work-item list --agent builder
holon work-item get WORK_ITEM_ID --agent builder
holon task status TASK_ID --agent builder
holon task output TASK_ID --agent builder
holon tail --agent builder --limit 10
holon transcript --agent builder --limit 30
```

<a id="tui-issues"></a>

### TUI 显示异常或连接失败

- 终端花屏或错乱：使用 `holon tui --no-alt-screen`（验证有效后可通过 `holon config set tui.alternate_screen never` 持久化）。
- 远程连接失败：核对地址与受保护的 Token 文件路径：

  ```bash
  holon tui --connect https://your-server:8787 --token-file /path/to/control-token
  ```

### 运行时性能快照与 Trace 诊断

在需要排查整体吞吐变慢时，使用控制面 Token 抓取性能快照：

```bash
curl --fail --silent --show-error \
  -H "Authorization: Bearer $HOLON_CONTROL_TOKEN" \
  http://127.0.0.1:7878/api/control/runtime/performance
```

对照[可观测性参考](../reference/observability.md#运行时性能快照)在同一守护进程生命周期内对比前后快照：

- `turn.tool_execution` 偏高：检查对应耗时命令与日志输出。
- 提供商重试频繁：排查上游报错、限流或网络连通性。
- `turn.context_build` 持续增长：检查受影响轮次的上下文体积与诊断信息。
- 空轮询计数偏高：核实当前是否真的存在 `runnable` 状态的工作项。

可通过 `holon debug trace --search MESSAGE_OR_TURN_ID` 检索保留的 Trace，或参阅[配置可观测性](configure-observability.md)校验 OTLP 导出链路。采样未完成前，避免仅为清零计数而重启守护进程。

### ACP 托管代理（`agy acp`）

使用 `acp` 执行驱动的 Agent 会在外部 `agy acp` 进程中运行模型轮次，由 Holon 监管该进程并裁决其权限请求。

1. 先用 `agy --version` 确认可执行文件，再运行 `agy acp --login` 完成登录。
   若无终端登录会话，可配置 `GEMINI_API_KEY` / `GOOGLE_API_KEY`，或通过
   `GOOGLE_APPLICATION_CREDENTIALS`、`GOOGLE_CLOUD_PROJECT`、
   `GOOGLE_CLOUD_LOCATION` 与 `AGY_ADC_AUTH` 提供 Vertex ADC 凭据。
2. `-32000` 表示缺少可用凭据。请先完成登录或配置上述环境变量；Holon 会把该轮次直接结算为结构化运行时错误，不会循环重试。
3. `-32002` 表示持久化的 ACP 会话已失效。Holon 会重建会话并记录 `acp_session_rotated` 事件，原有的会话级 `allow_always` 授权也会随之失效。
4. 默认的 `unattended` 权限模式不会等待人工响应。如需在控制台处理权限审批与表单输入，请将 `acp.profiles.<profile>.permission_mode` 设为 `interactive`，并按需调整 `acp.interaction_timeout_ms`，再携带控制面凭据调用 `GET/POST /api/control/agents/{agent_id}/acp-interactions`。超时未答复的请求会自动拒绝；对已结算的 ID 重复提交会返回 `409`（附带 `settled_reason`），未知 ID 返回 `404`。`interactive` 模式比 `unattended` 更严格：即使是工作区根目录内的操作也会等待审批，超时即拒绝。仅有 `allow_always` 会在当前会话内保留授权，`reject_always` 只作用于单次请求。
5. 计划、外部托管工具内部子任务和上下文用量更新会记录为 `acp_plan_updated`、`acp_subagents_updated` 与 `acp_usage_updated` 事件。它们只是只读观测记录，托管进程内的 subagent 不会进入 Holon 自身的监管树；这些事件会在每轮结束结算时统一发出。

完整操作清单可参阅仓库内的 `docs/local-operator-troubleshooting.md`。

## 何时停止重试并上报阻塞

若完成上述分层检查仍无法定位或修复，请停止盲目重试并整理以下信息上报：

- Agent ID、WorkItem ID、Task ID 以及相关 Message/Turn ID。
- 预期行为与实际观察到的调度/错误状态（附脱敏后的精确报错）。
- 已执行过的只读核查步骤，以及已确认的外部副作用。

## 另见

- [实时观察、追踪与验收 Agent 工作进展](observe-agent-work.md)
- [将复杂任务安全委派给子 Agent](delegate-work.md)
- [运行长生命周期任务](run-long-lived-task.md)
- [调度 Agent 工作](schedule-agent-work.md)
- [配置参考](../reference/configuration.md)
