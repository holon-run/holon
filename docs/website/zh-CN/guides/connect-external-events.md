---
title: 连接外部事件
summary: 配置回调端点、登记等待条件，并验证外部事件能够如期唤醒目标工作。
order: 100
---

# 连接外部事件

通过 HTTP 回调，可以在 CI 完成或外部状态发生变化时及时通知 Agent。本指南以“唤醒回调（Wake callback）”为例进行演示：外部事件到达时唤醒 Agent 检查外部状态，再由 Agent 自主判定工作项是否能够继续推进。

## 前置条件

- 正常运行中的 Agent，拥有一个被持续跟踪的目标，以及需要关注的外部对象（例如某个具体的 CI 构建）。
- 赋予该 Agent 对该外部数据源的只读检查权限，并在指引中明确成功与失败后的处理规则。
- 确保外部系统能够网络访问该回调 URL。Loopback 地址仅对本机有效；若是远程外部服务，需配置好经过批准的远程访问或隧道。
- 回调 URL 属于机密凭据。在操作过程中避免在 Shell 历史记录或详细 HTTP 日志中打印明文。
- 以下由操作者执行的示例依赖 `curl`、`jq` 以及宿主机上的控制平面访问权限。

## 操作步骤

1. 获取 Agent 当前的默认接入凭据：

   ```bash
   AGENT_ID=builder
   TOKEN=$(jq -r .token ~/.holon/credentials.json 2>/dev/null || echo "$HOLON_CONTROL_TOKEN")

   curl -s -H "Authorization: Bearer $TOKEN" \
     "http://127.0.0.1:7878/api/control/agents/$AGENT_ID/reset-callback" -X POST
   ```

   该操作会吊销旧的默认凭据并返回包含新 `trigger_url` 的 JSON 响应。请妥善保存该 URL。

2. 在外部系统中配置 Webhook：
   将获取到的 `trigger_url` 填入 CI 系统（如 GitHub Actions、GitLab CI）的 Webhook 通知配置中，设置为在构建结束时触发 HTTP POST。对于唤醒模式（`wake_hint`），空的 POST 请求体即可生效。

3. 让 Agent 启动任务并登记等待：
   要求 Agent 触发构建并等待其结果。在底层，Agent 会记录工作项、触发外部构建，并执行如下等待调用：

   ```json
   {
     "wake": "external",
     "resource": "github:holon-run/holon#run/12345",
     "reason": "等待 GitHub Actions CI 构建 #12345 完成",
     "delivery": "final"
   }
   ```

   随后 Agent 进入挂起状态，让出当前轮次的执行权。

4. 触发测试唤醒：
   在外部 CI 触发前，可手动发送一条空的 POST 请求模拟外部通知：

   ```bash
   curl -X POST "$TRIGGER_URL"
   ```

5. 验证唤醒与后续执行：
   观察守护进程状态与 Agent 日志：

   ```bash
   holon agent get $AGENT_ID
   holon transcript --agent $AGENT_ID --limit 10
   ```

   应该能观察到 Agent 被该次外部事件唤醒，随后主动调用工具查询 CI 的真实状态，并根据查询到的结果继续推进。

## 确认配置成功

- 回调 URL 正确返回 `200 OK` 或受理状态。
- 处于挂起等待状态的工作项在收到通知后被正确唤醒。
- Agent 没有盲目轻信 Webhook，而是主动核实了对应的外部构建记录与 Commit SHA。

## 凭据轮换与清理

当集成不再使用或凭据意外暴露时，应立即进行轮换或吊销：

```bash
# 重新重置并生成新凭据（旧凭据立即失效）
curl -s -H "Authorization: Bearer $TOKEN" \
  "http://127.0.0.1:7878/api/control/agents/$AGENT_ID/reset-callback" -X POST
```

停止 Agent 同样会自动吊销其绑定的外部触发器。

## 另见

- [事件、等待与唤醒概念](/zh-CN/concepts/events-and-waits.md) — 异步心智模型
- [事件与定时器参考](/zh-CN/reference/events-and-timers.md) — 端点与字段契约
- [调度 Agent 工作](schedule-agent-work.md) — 基于时间的定时调度
