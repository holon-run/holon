---
title: 用自托管 changedetection.io 让网页变更唤醒 Holon agent
summary: "自托管 changedetection.io 监控任意网页，检出变更后通过 webhook 回调唤醒 Holon agent：Docker 部署、watch 与通知配置、端到端验证，全程本地闭环。"
order: 189
---

# 用自托管 changedetection.io 让网页变更唤醒 Holon agent

changedetection.io 是开源（Apache-2.0）的网页变更监控工具：你告诉它盯住哪个 URL，它在内容变化时通过通知渠道（含自定义 webhook）告诉你。把它接到 Holon 的外部回调入口后，「某个网页变了」就变成一条能唤醒 agent 的事件——agent 被唤醒后可以自己去读变更内容、更新任务、或按你预先写好的规则行动。本文给出从部署到端到端验证的完整流程，全部步骤在一台机器上即可复现，不需要任何外部账号。

本文场景对应 Holon 的[连接外部事件](./connect-external-events.md)工作流：webhook 只是一个唤醒信号（`wake_hint`），agent 醒来后检查外部状态再决定下一步。changedetection.io 0.70 版本还内置了 MCP server 接口，可作为工具面接入，本文不展开。

## 前置条件

- 一台能跑 Docker 的机器。
- Holon 0.59.0 或更高（需支持外部回调入口 `POST /api/callbacks/wake/:token` 与 `holon agent reset-callback`）。
- changedetection.io 0.70.3（2026-11-10 实测版本）。

## 第 1 步：部署 changedetection.io

```bash
docker run -d --name changedetection \
  --network host \
  -p 127.0.0.1:5000:5000 \
  -v "$PWD/cdet-data:/datastore" \
  dgtlmoon/changedetection.io:0.70.3
```

两个实测要点：

- **数据卷挂载点是 `/datastore` 而不是 `/data`**。0.70.3 镜像内的数据目录是 `/datastore`（每个 watch 一个 `<uuid>/watch.json`，外加全局 `changedetection.json`）。挂错到 `/data` 不会有任何报错，重启后配置全部丢失——这是最常见的坑。
- 如果要监控**内网/本机地址**（例如同一台机器上的自建服务），需要额外加 `-e ALLOW_IANA_RESTRICTED_ADDRESSES=true`。这是 changedetection.io 的 SSRF 防护开关（GHSA-rph4-96w6-q594）：默认拒绝 loopback/私有地址目标，防止把工具变成内网探测跳板。只在隔离测试环境打开，生产实例不要开。

启动后访问 `http://<host>:5000` 即管理界面。界面无需登录（可用环境变量另设密码），所有操作也可以通过 REST API（`/api/v1/...`，需在 Settings 里生成 API key）完成。

## 第 2 步：准备 watch 目标

本文用同机的一个静态页演示。生产场景可以是任意你能访问的 URL：文档页、发布页、价格页、issue 列表等。

```bash
mkdir -p /srv/watch-target
printf '<html><body><h1>status page v1</h1></body></html>\n' > /srv/watch-target/page.html
python3 -m http.server 8088 --directory /srv/watch-target --bind 127.0.0.1 &
```

在 changedetection.io 界面「Add a watch」填入 `http://127.0.0.1:8088/page.html`，保存后它会立即做第一次快照作为基线。检查节奏默认 3 小时一次，可在 watch 设置里调小（本例为了演示用手动触发：列表页点「Check now」，或 `POST /checknow?uuid=<watch-uuid>`）。

## 第 3 步：获取 Holon 回调能力

每个 Holon agent 都有一个默认外部回调入口；为隔离实验建议给一次性测试 agent 单独签发（`reset` 会吊销旧 URL 并生成新的）：

```bash
holon agent reset-callback --agent <agent-id> > callback.json
chmod 600 callback.json
jq -r '.delivery_mode' callback.json   # 期望 wake_hint
jq -r '.trigger_url' callback.json     # 即回调 URL，按机密处理，不要写进任何文档/日志
```

回调 URL 是 capability secret：任何持有者都能唤醒该 agent。它只应存在于发送方（changedetection.io）的配置里；本文以下一律用 `<wake-url>` 占位。

agent 侧在等待期间注册外部等待（这是 agent 工具调用，不是 HTTP 配置）：

```json
{"wake": "external", "resource": "cdet:watch:<watch-uuid>", "reason": "网页变更后检查最新内容", "delivery": "silent"}
```

## 第 4 步：配置 watch 的 webhook 通知

编辑 watch，在 Notifications 区填写：

| 字段 | 实测值 | 说明 |
|---|---|---|
| Notification URL | `json://127.0.0.1:7878/api/callbacks/wake/<token>` | apprise JSON 渠道，即把 `<wake-url>` 的 `http://` 换成 `json://`。apprise 会向该地址发 JSON POST |
| Notification Title | `changedetection: {{watch_title}} changed` | 模板变量在 apprise 侧渲染 |
| Notification Body | `watch {{watch_url}} detected a change; event id {{watch_uuid}}` | 随 payload 发出 |
| Notification Format | `text` | **必须小写**；填 `Text` 会在发送时报 `Invalid notification format: "Text"`，通知静默失败 |

通知格式合法值为 `text` / `html` / `htmlcolor` / `markdown` / `System default`（`System default` 默认 `htmlcolor`）。`json://` 渠道最终 POST 的 payload 形如：

```json
{"version": "1.0", "title": "...", "message": "...", "attachments": [], "type": "info"}
```

## 第 5 步：端到端验证

1. 修改目标页内容（v1 → v2），点「Check now」。
2. changedetection.io 日志应出现 `Change detected in UUID <uuid>`（实测从触发到检出 <1s），随后 `Notifying 1 service(s)` 与完整 `JSON POST URL: ... /api/callbacks/wake/...` 日志。
3. agent 被 wake 唤醒续跑：`holon tail --agent <agent-id> --include-progress` 可见新 turn 与「外部唤醒已到达」进度；agent 自查目标页确认新内容后交付结论。

反向用例实测（0.70.3 × Holon 0.59.0，2026-11-10）：

| 用例 | 行为 |
|---|---|
| 无效回调 token | Holon 返回 403 + `{"code":"forbidden","error":"invalid callback token","ok":false}`，不唤醒 |
| watch 目标 404 | 不视为变更、不发通知（保留旧快基线，日志 `checksum ... the same, nothing todo`），无误报 |
| 通知端点超时 | apprise 记录 `Connection error ... Read timed out`，watch 状态页可见失败记录 |

## 已知问题：wake 回调响应延迟

验证中发现 Holon `POST /api/callbacks/wake/:token` 对有效 token 不返回 HTTP 响应（发送方 4s/30s 超时均收不到 ack），delivery 实际生效但 agent 唤醒延迟可达分钟级，详见 [holon-run/holon#4016](https://github.com/holon-run/holon/issues/4016)。在该问题修复前：

- changedetection.io（apprise 默认 4s 超时）每次通知都会记录「失败」，但事件实际已送达，不要据此重配 URL；
- 如需延长 apprise 超时，可在通知 URL 追加超时参数（apprise 通用参数）降低误报噪音；
- 需要精确判断是否送达时，以 agent 侧 tail/状态为准，不要依赖 HTTP 状态码。

## 排错速查

| 现象 | 原因与处理 |
|---|---|
| 重启后 watch 全部丢失 | 数据卷挂到了 `/data`；改挂 `/datastore` |
| 添加 watch 报 400（自动化/API 场景） | 表单有 CSRF 保护：先 GET 页面取 `csrf_token` 与 session cookie 再 POST |
| 添加本机 URL 报不可达/拒绝 | SSRF 防护拦截；隔离环境加 `ALLOW_IANA_RESTRICTED_ADDRESSES=true`（生产勿开） |
| 通知报 `Invalid notification format: "Text"` | 格式字段必须用小写合法值（`text`/`html`/`markdown` 等） |
| 通知报 `Read timed out` 但 agent 确实被唤醒 | Holon #4016 的响应挂起问题，见上一节 |
| agent 没被唤醒 | 确认 agent 已注册 `WaitFor(wake=external)` 且未停止；停止的 agent 会吊销其全部外部 trigger，发送方保存的旧 URL 随之失效 |

## 清理

验证结束后：删除 watch 与容器（`docker rm -f changedetection` 及数据卷），删除测试页进程与文件；`reset-callback` 签发的 trigger 会随 agent 停止自动吊销，签发时保存的 `callback.json`（含明文 URL）应删除。
