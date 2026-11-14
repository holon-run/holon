---
title: 实时观察、追踪与验收 Agent 工作进展
summary: 在 Web GUI 中实时追踪 Agent 执行状态与动态 Todo 清单，在自然语言对话中排查等待门禁，并验收权威完成简报与交付产物，无需 SSH 登录终端排查日志。
order: 250
---

# 实时观察、追踪与验收 Agent 工作进展

当 AI Agent 执行复杂且耗时的工作（例如审计未知的遗留代码库、重构核心子系统、或等待多轮测试套件执行）时，你需要随时掌握它的实时进度。你不需要在服务器上打开终端 SSH 登录、盯生硬的 NDJSON 日志流，或者猜测 Agent 到底是在正常执行、等待确认还是陷入了死循环。

在 Holon 中，Agent 的所有长周期任务都承载在受 SQLite WAL 状态机保护的持久化 **工作项（WorkItems）** 之上。你可以直接在 Web GUI 或移动端界面中，通过自然语言对话询问进度、在侧边栏实时跟踪动态 Todo 清单勾选、审查生成的文件产物，并核验最终完成报告。

本指南介绍如何在对话中追问 Agent 进展、解读关键调度状态、在 Web GUI 中核验产物，以及如何验收权威完成简报。关于底层的状态机契约，请参阅[工作项参考](../reference/work-items.md)与[上下文延续机制](../concepts/context-continuity.md)。

## 前置条件

- 正在运行的 Holon 实例与 [Web GUI](./use-web-gui.md)（默认地址 `http://localhost:8080`）。
- 处于活跃状态的 Agent（例如 `developer` 或 `main`）。
- 正在推进的具体任务（已创建持久化 WorkItem，参见[运行无须值守的长周期任务与恢复](./run-long-lived-task.md)）。
- 无须 SSH 登录服务器或敲终端命令：日常进度查询、清单核验与产物验收全部在 Web GUI 对话与侧边栏中完成。

## 操作步骤

### 1. 在对话中随时询问实时进展与等待状态

了解 Agent 当前工作最快捷的方式就是在 Web GUI 聊天窗口中直接提问：

> “汇报一下当前的执行进展，是否有正在等待的任务或阻塞？”

Agent 会立即检查当前 WorkItem 的执行状态与等待条件，并回复清晰的结构化摘要：

- **当前正在推进的里程碑**：明确当前正在阅读哪部分代码、编写哪个测试用例或排查何种错误。
- **当前调度状态与等待门禁**：
  - **正在执行（Runnable / Executing）**：Agent 正在分析上下文、生成代码补丁或执行短命令。
  - **等待后台任务（Waiting on Task，`wake=task_result`）**：耗时较长的后台命令（如编译整个项目、运行测试套件、下载大依赖）正在受管执行中。Agent 处于零 Token 休眠状态，任务结束后会自动被系统唤醒。
  - **等待操作者确认（Waiting for Operator，`wake=operator_input`）**：Agent 遇到了架构设计抉择、高影响操作门禁（如 Git 推送、数据库破坏性变更）或缺少必要的密钥凭证，正在等待你的决策输入。
  - **等待外部事件或定时器（Waiting on Timer / External Event）**：Agent 正在等待预定的定时轮询周期（参见[让 Agent 执行定时巡检与周期性任务](./schedule-agent-work.md)）或 Webhook 回调通知。
- **后续待办项**：当前工作项 Todo 清单中剩余的步骤。

### 2. 在 Web GUI 侧边栏实时核验动态 Todo 清单与产物文件

除了自然语言问答，Web GUI 会实时投影 Agent 的内部状态：

1. **打开工作项侧边栏面板**：点击 Web GUI 顶栏或侧栏的 **Work Items** 图标。
2. **查看动态 Todo 清单**：
   - 实时观察各项待办的状态：已完成（`completed`）、正在进行（`in_progress`）或待处理（`pending`）。
   - 随着 Agent 推进文件修改与测试验证，这些复选框会自动打钩更新，无需手动刷新网页。
3. **查看持久化计划文档（`plan.md`）**：
   - 点击计划文档链接，阅读 Agent 针对该任务梳理的范围边界、验收指标与架构决策记录。
4. **即时预览生成产物（Artifacts）**：
   - 当 Agent 生成代码补丁、测试报告或草稿文档时，会将其登记为权威产物。点击对话流或侧边栏中的产物链接，可直接在浏览器中查看语法高亮的代码 Diff 或 Markdown 渲染视图。

### 3. 当任务暂停或受阻时的诊断与人工干预

如果 Agent 进入暂停状态（`waiting_operator` 或计划状态为 `needs_input`），它不会盲目猜测，而是会停下来等待授权：

1. **阅读暂停原因说明**：Agent 会在对话中清楚陈述停下来的原因（例如：“*检测到该迁移需要删除现存的旧数据列，请确认是直接删除还是保留归档？*”）。
2. **直接在对话中给出明确决策**：
   > “确认删除未使用的旧数据列，并同步更新相关的 Schema 单元测试。”
3. **自动解除等待**：提供输入后，运行时的 `WaitFor(wake="operator_input")` 自动满足，Agent 将计划更新为 `ready`，即刻苏醒并继续执行，无需重启或重新投递任务。
4. **干预失控或偏航任务**：如果你发现 Agent 的实现方向偏离了预期，直接在对话中下达停止或调整指令：
   > “停止当前的重试循环。先撤销对 `auth.rs` 的最后一次修改，我们重新梳理令牌校验逻辑。”

### 4. 严格验收权威完成简报与交付物

当 Agent 完成所有清单项并通过相关测试后，会调用 `CompleteWorkItem` 归档工作项，并在 Web GUI 中发布最终的**权威完成简报（Completion Brief）**：

在验收完成简报时，重点核对以下四个核心要素：

1. **变更摘要**：明确说明修改了哪些文件、新增了哪些模块或调整了哪些配置。
2. **验证凭证**：检查简报中列出的实际测试命令（如 `cargo test --all`、`npm run build`）及其退出状态（`0`）。绝不接受口头声称成功却缺乏真实构建与测试依据的简报。
3. **交付产物路径**：确认生成的代码补丁、文档或工作区路径位于当前真实有效的 Git Worktree 中。
4. **注意事项与后续建议**：Agent 标注的环境前提、未覆盖的边缘边界或建议的后续人工复核事项。

确认无误后，即可在 Git Worktree 中完成分支合并（参见[在项目工作区与 Git Worktree 中工作](./work-with-workspaces.md)）。

### 5. 可选：面向自动化运维的 CLI 与 HTTP 审计接口

如果你正在编写自动化 CI/CD 脚本、定时监控脚本或无头运维看板，Holon 提供了完整的非交互式查询接口。

**通过 CLI 查看最新简报与实时进度：**
```bash
# 查看最近的交付简报
holon tail --agent developer --limit 10

# 抓取结构化的最近进度事件流
holon tail --agent developer --include-progress --limit 20
```

**检查活跃工作项与后台任务：**
```bash
# 列出指定 Agent 的所有工作项及状态
holon work-item list --agent developer

# 查看特定工作项详情与 Todo 清单
holon work-item get WORK_ITEM_ID --agent developer

# 检查具体后台命令任务的运行状态
holon task status TASK_ID --agent developer
```

关于完整的 HTTP 接口端点，请参阅[通过 HTTP 自动化操作](./automate-over-http.md)。

## 后续探索

- [运行无须值守的长周期任务与恢复](./run-long-lived-task.md)：深入了解 WorkItems 如何在断网、重启后保持任务零漂移恢复。
- [在项目工作区与 Git Worktree 中工作](./work-with-workspaces.md)：审查并安全清理 Agent 在独立 Worktree 中产出的代码变更。
- [让 Agent 执行定时巡检与周期性任务](./schedule-agent-work.md)：为 Agent 配置自动化定时器，执行日常代码巡检与文档维护。
- [排查故障](./troubleshooting.md)：排查模型提供商超时、任务卡死与权限异常问题。
