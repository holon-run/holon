---
title: 通过 HTTP 自动化执行
summary: 鉴权接入、提交单一跟踪目标、跟进执行状态并获取完成报告。
order: 120
---

# 通过 HTTP 自动化执行

本指南演示如何通过操作者控制平面提交 WorkItem、保存其 ID 并获取最终报告。外部事件通知请使用公开的 enqueue 或 callback 接口；这些接口不赋予操作者控制权限。

## 开始之前

准备好 `curl`、`jq`、已配置好的模型，以及具备目标工作区访问权限的已有 Agent。本示例中使用 `builder` 并由操作者在终端执行。切勿将控制平面 Token 分发给不受信任的外部 Webhook 发送方。

## 启动服务

若需启动独立的本地服务，先将 Token 安全写入 `HOLON_CONTROL_TOKEN`，随后在独立终端中启动服务：

```bash
: "${HOLON_CONTROL_TOKEN:?请安全设置控制 Token}"
HOLON_CONTROL_TOKEN="$HOLON_CONTROL_TOKEN" holon serve --port 8787
```

若守护进程已启用了 HTTP，直接使用其地址和 Token，无需启动第二个服务。默认地址为 `127.0.0.1:7878`；本指南中使用端口 `8787`。绑定非回环地址或开启 `lan`/`tailnet` 访问均强制要求身份鉴权。本地测试请保持在回环地址，并在处理机密变量时关闭 Shell 执行回显。

## API 调用约定

在发送请求的终端中定义以下环境变量：

```bash
BASE=http://127.0.0.1:8787
AGENT=builder
: "${HOLON_CONTROL_TOKEN:?请安全设置控制 Token}"

api() {
  curl --fail --silent --show-error     -H "Authorization: Bearer $HOLON_CONTROL_TOKEN"     -H 'Content-Type: application/json' "$@"
}

api "$BASE/api/agents/$AGENT/status"
```

仅在成功返回目标 Agent 信息后继续。下文的所有路由均包含 `/api` 前缀。请求与响应的具体 Schema 请参阅 [HTTP 参考](../reference/http-control-plane.md)。

## 接口示例

### 创建 Agent

本指南使用已有 Agent。如需新建，请先参阅[从模板创建 Agent](create-agent.md)与[在 Workspace 中工作](work-with-workspaces.md)，随后重复上述状态检查。通过 HTTP 创建 Agent 的接口定义在 [HTTP 参考](../reference/http-control-plane.md)中。

### 创建工作项

提交一次并保存返回的记录：

```bash
WORK=$(api -X POST "$BASE/api/control/agents/$AGENT/work-items"   -d '{"objective":"只读检查附加的项目。识别其文档中记录的测试命令，确认其定义位置，并产出报告完成该工作项。不要编辑文件、不要运行测试、不要发布任何内容。"}')
WORK_ID=$(printf '%s' "$WORK" | jq -er '.id')
printf 'WorkItem: %s
' "$WORK_ID"
```

成功的响应代表目标已被系统接收并持久化记录，并不代表 Agent 已经完成了工作。若请求在提交后超时，请先列出该 Agent 的所有工作项进行目标匹配再决定是否重试；盲目重试会导致创建重复的工作项。

### 检查 Agent 状态

针对该具体工作项进行状态查询，切勿把通用的 Agent 状态当作工作项的完成信号：

```bash
api "$BASE/api/agents/$AGENT/work-items/$WORK_ID"   | jq '{id, objective, state, plan_status, blocked_by, result_brief_id, result_summary}'
```

在自动化脚本中应设置有界的等待窗口。以下示例最多等待一分钟，并在需要人工介入时提前退出：

```bash
for attempt in $(seq 1 12); do
  WORK=$(api "$BASE/api/agents/$AGENT/work-items/$WORK_ID") || break
  STATE=$(printf '%s' "$WORK" | jq -r '.state')
  PLAN=$(printf '%s' "$WORK" | jq -r '.plan_status')
  [ "$STATE" = completed ] && break
  [ "$PLAN" = needs_input ] && break
  sleep 5
done
printf '%s
' "$WORK" | jq .
```

超时后工作项会保留在其实际状态；切勿单纯为了让自动化流水线变绿而强行将其标记为完成。若需要决策，请查阅最新简报并通过授权的操作者渠道进行答复。

当 `state` 变为 `completed` 时，拉取关联的报告：

```bash
BRIEF_ID=$(printf '%s' "$WORK" | jq -er   'select(.state == "completed") | .result_brief_id // empty')
api "$BASE/api/agents/$AGENT/briefs/$BRIEF_ID"
```

仅在成功提取到 ID 时发起请求。核查报告是否准确回应了目标并给出了相应证据。对于生成文件的任务，还要核实返回的产物存储位置。

### 更新与完成工作项

尽量由执行中的 Agent 自行更新并完成其工作。操作者侧的 PATCH 与 completion 端点主要用于显式的生命周期管理，但一个处于活跃状态的工作项不能被无关的控制请求随意完成。完成操作必须提供非空的完成报告以及验收证据。在使用这些修改接口前，请参阅[工作项核心操作参考](../reference/work-items.md#核心操作)。

### 向 Agent 发送消息

接收外部信息（例如通知检查 CI 运行状态）时，请使用公开的入队（enqueue）接口：

```bash
api -X POST "$BASE/api/agents/$AGENT/enqueue"   -d '{"text":"CI 运行状态已变更，请核查最新进展。","priority":"normal","origin":{"kind":"webhook","source":"ci-service"}}'
```

接收成功的响应包含消息 ID，但并不证明请求的排查动作已经执行完毕。公开入队接口仅接受 `channel` 或 `webhook` 类型的来源，拒绝调用方伪造的 trust 标识或 `interject` 优先级，并会严格保留外部来源属性。操作者协议绑定请参阅 HTTP 参考，不可把外部报文伪造为操作者指令。

### 创建与取消计时器（Timer）

有关定时跟进任务，请参阅[调度 Agent 工作](schedule-agent-work.md)。该指南涵盖约定动作、创建计时器、校验与取消操作。[事件参考](../reference/events-and-timers.md)列出了相关 HTTP 路由。单纯创建计时器不代表已经登记了等待。

### 唤醒休眠的 Agent

针对外部发送方，使用[连接外部事件](connect-external-events.md)中介绍的 Callback 凭据能力。唤醒操作不会清除独立的业务阻塞项，也不会授予额外动作权限。操作者唤醒端点与 Payload 结构请参阅 [HTTP 参考](../reference/http-control-plane.md)。

### 列出 Agent 任务

```bash
api "$BASE/api/agents/$AGENT/tasks"
```

在读取任务输出前，先匹配任务 ID 与工作项归属。任务执行完毕与工作项目标完成属于两项独立的检查。

### 获取 Agent 执行轨迹（Transcript）

```bash
api "$BASE/api/agents/$AGENT/transcript?limit=30"
```

当完成报告与状态无法解释故障原因时，使用有界读取查看执行轨迹。分享诊断输出前请脱敏凭据与无关对话。

## 故障排查

- **鉴权失败：** 检查服务端运行模式、Token 准确性与目标访问地址。
- **Agent 未找到：** 核对 ID 与调用者权限；切勿对拼写错误的 ID 盲目创建替代实例。
- **已接收但未完成：** 检查工作项状态、最近的简报以及任务状态。
- **修改操作后连接断开：** 在重试前先查询持久化状态。
- **已完成但缺少可用报告或产物：** 保留相关 ID 并如实报告不一致问题；切勿单纯依赖 HTTP 状态码推断业务执行成功。

## 另见

- [HTTP 控制平面参考](../reference/http-control-plane.md)
- [观察 Agent 工作](observe-agent-work.md)
- [信任边界](../concepts/trust-boundaries.md)
