---
title: 用自托管 GlitchTip 把错误告警唤醒 Holon
summary: "自托管 GlitchTip 作为错误追踪：错误经 Sentry 兼容摄入生成 issue，项目告警 webhook 唤醒 Holon，agent 再用 REST API 核对 issue 与事件。不改 Holon 运行时代码。"
order: 189
---

# 用自托管 GlitchTip 把错误告警唤醒 Holon

GlitchTip 是 Sentry 兼容的开源错误追踪（后端与前端仓库均为 MIT）。Holon 不需要专门的 GlitchTip 适配：项目告警的通用 webhook recipient 把错误摘要 POST 到 Holon 的 wake callback，agent 再用项目 API token 拉取 issue 和事件详情。

本文只覆盖这条路径。它不是 OIDC，也不替代应用自己的 SDK 接入。

## 实测版本与结论

2026-11-16 在隔离 Docker Compose 环境实测通过，**不需要修改 Holon 运行时代码**。

- GlitchTip 镜像 `glitchtip/glitchtip:latest`，digest `sha256:c245745b4179667da3aebd73fb8d4f1fd918f7d68ba07d1fd73ea688933b6e01`（镜像创建时间 2026-11-09）。投递请求的 User-Agent 为 `GlitchTip/7.0.0`。
- Holon `0.59.0`（`33e2fcb4b`）。
- 正向：`POST /api/{project_id}/store/?sentry_key=<DSN public key>` 摄入一条错误后，约一个告警周期内 webhook 到达，Holon `POST /api/callbacks/wake/<token>` 返回 200。REST `GET /api/0/issues/1/` 的 `title` 与 webhook `attachments[0].title` 一致（本次均为 `ProbeError: glitchtip webhook verification`）。`GET /api/0/issues/1/events/` 的 `event_id` 与摄入响应一致。
- 反向：无 token 或无效 Bearer token 均为 `401 {"detail":"Unauthorized"}`。`quantity` 高于窗口内事件数时不投递、也不唤醒。无效 recipient URL 不会打到 Holon。

## 前置条件

- 能跑 Docker 的机器。GlitchTip 只监听测试网或本机，不要把 8080 暴露到公网。
- Holon 已有一个 `wake_hint` callback URL。该 URL 是 capability secret，只填进 GlitchTip 的 recipient，不要写入文档、计划、日志或截图。
- 若 callback 主机是内网、回环或 tailscale 的 `100.64.0.0/10`，必须打开 `GLITCHTIP_ALLOW_PRIVATE_IPS`。GlitchTip 把 CGNAT 段视为不可投递地址；不打开时，创建 recipient 会被拒绝，或在发送时被 SSRF 防护丢掉。

## 第 1 步：部署

当前 `latest` 镜像是单进程架构：`./bin/start.sh` 同时提供 HTTP、消费任务队列，并在启动时初始化数据库。不要再按旧文档拆 `migrate` / `worker` 服务；镜像里的 `bin/run-worker.sh` 会直接拒绝运行。

队列变量以 `VALKEY_URL` 为准，`REDIS_URL` 只是兼容回退。`redis:7` 协议兼容，可直接使用。HTTP 端口由 `PORT` 控制，下面映射到本机 `127.0.0.1:8080`。

```yaml
services:
  postgres:
    image: postgres:16-alpine
    environment:
      POSTGRES_DB: glitchtip
      POSTGRES_USER: glitchtip
      POSTGRES_PASSWORD: "<数据库密码>"
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U glitchtip -d glitchtip"]
      interval: 5s
      retries: 20
  redis:
    image: redis:7-alpine
  web:
    image: glitchtip/glitchtip:latest
    ports:
      - "127.0.0.1:8080:8080"
    environment:
      DATABASE_URL: "postgres://glitchtip:<数据库密码>@postgres:5432/glitchtip"
      VALKEY_URL: "redis://redis:6379/0"
      SECRET_KEY: "<随机长密钥>"
      PORT: "8080"
      GLITCHTIP_DOMAIN: "http://glitchtip.example.com:8080"
      ENABLE_ORGANIZATION_CREATION: "true"
      GLITCHTIP_ALLOW_PRIVATE_IPS: "true"
    depends_on:
      postgres:
        condition: service_healthy
volumes:
  pgdata:
```

`GLITCHTIP_DOMAIN` 必须是人和 agent 最终访问的地址。不设置时 webhook 里的 `title_link` 和 API 的 `permalink` 会落成默认的 `http://localhost:8000/...`，本次实测就是这个默认值。只在 callback 确实指向内网时才保留 `GLITCHTIP_ALLOW_PRIVATE_IPS=true`。

健康检查：`GET /_health/` 返回 200。

## 第 2 步：建用户、项目和 API token

当前镜像 `HEADLESS_CLIENTS` 只有 `browser`，没有 app token 客户端。注册走 headless browser API，需要先取 CSRF cookie：

1. `GET /_allauth/browser/v1/config`
2. `POST /_allauth/browser/v1/auth/signup`，JSON 为 `{"email","password"}`，带头 `X-CSRFToken` 和 `Referer`。默认未配置邮件时 `ACCOUNT_EMAIL_VERIFICATION` 为 `none`，注册即登录并下发 `sessionid`。
3. 用同一 session：`POST /api/0/organizations/` `{"name":"..."}`，`POST /api/0/organizations/{org}/teams/` `{"slug":"..."}`，`POST /api/0/teams/{org}/{team}/projects/` `{"name":"...","platform":"python"}`。
4. `POST /api/0/api-tokens/`，session 鉴权，body 示例：`{"label":"holon","scopes":["project:read","project:write","event:read","event:write","org:read"]}`。响应里的 `token` 只出现这一次，当作 secret。
5. `GET /api/0/projects/{org}/{project}/keys/` 读取 DSN。`public` 字段是 UUID；摄入时去掉连字符，作为 `sentry_key`。

之后的查询用 `Authorization: Bearer <token>`，不再依赖 session。

## 第 3 步：配置告警 recipient

`POST /api/0/projects/{org}/{project}/alerts/`：

```json
{
  "name": "holon-error-alert",
  "timespanMinutes": 60,
  "quantity": 1,
  "uptime": false,
  "alertRecipients": [
    {"recipientType": "webhook", "url": "http://<holon-host>:<port>/api/callbacks/wake/<token>"}
  ]
}
```

`recipientType` 必须是 `webhook`（通用 webhook）。`quantity` 是窗口内事件数阈值，`timespanMinutes` 是窗口。两者都要有值，否则这条告警不会被周期任务选中。默认检查间隔是 `ALERT_NOTIFICATION_INTERVAL` 秒，未设置时为 60。

## 第 4 步：摄入一条错误

```bash
curl -sS -X POST "http://127.0.0.1:8080/api/1/store/?sentry_key=<dsn-public-key>"   -H 'Content-Type: application/json'   -d '{"event_id":"<uuid>","message":"probe","level":"error","platform":"python","exception":{"values":[{"type":"ProbeError","value":"glitchtip webhook verification"}]}}'
```

成功返回 `{"event_id":"..."}`。issue 由队列异步生成，告警在下一个周期发出。

## webhook 与 API 的对应关系

通用 webhook 不是原始 Sentry 事件，而是摘要：

```json
{
  "text": "GlitchTip Alert",
  "attachments": [
    {
      "title": "ProbeError: glitchtip webhook verification",
      "title_link": "http://glitchtip.example.com:8080/holon-test/issues/1",
      "fields": [{"title": "Project", "value": "holon-probe", "short": true}]
    }
  ]
}
```

Holon wake callback 是 `wake_hint`：HTTP 200 只表示 runtime 接受了唤醒，请求体不会被当成 operator 指令。agent 被唤醒后应自己查 API：

- `GET /api/0/issues/{id}/`：`title` 对齐 webhook 的 `attachments[].title`，`metadata.type` / `metadata.value` 对齐异常。
- `GET /api/0/issues/{id}/events/`：`event_id` 对齐摄入响应，`entries` 里有 exception 和 message。

webhook 里没有 `event_id`。要对单条事件，用 issue id 再查 events，不要假设 webhook 体就是完整事件。

## 反向用例与限制

- 不带 `Authorization`，或 Bearer token 无效：`401 {"detail":"Unauthorized"}`。
- 窗口内事件数小于 `quantity`：不投递、不唤醒。本次 `quantity=5` 且只有 1 条事件，下一个 60 秒周期没有第二次 webhook，也没有第二次 callback。
- 无效 recipient（关闭的端口）不会唤醒 Holon。但 GlitchTip 7 的 `POST .../alerts/{id}/test/` 仍返回 `status: sent`，连接失败在 `_post` 里被吞掉且不写日志。不能用测试接口的 `sent` 或实例日志判断投递成功；以 Holon access log 的 200 和本机捕获为准。
- 创建 recipient 时如果 URL 指向被拦截的地址，API 会直接拒绝。内网 callback 忘记开 `GLITCHTIP_ALLOW_PRIVATE_IPS` 会卡在这一步，而不是运行后静默失败。

## 清理

`docker compose down -v` 会删除数据库、项目、token 和 recipient。不要把 callback URL 留在已停止但未删卷的库里。

## 未覆盖

- 邮件、Discord、Slack 等其他 recipient。
- 去抖之外的重复告警策略，以及 uptime monitor 告警。
- 浏览器 UI 建项目。本次全部走 HTTP API，没有使用 UI。
