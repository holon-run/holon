# pre-public runtime / AgentInbox 联调执行手册（2026-04）

这份手册是在
[runtime-incubation-agentinbox-integration-test-plan-2026-04.md](./runtime-incubation-agentinbox-integration-test-plan-2026-04.md)
基础上的可执行版本。

目标是让我们能用当前真实仓库和当前真实 CLI，跑通第一轮最小闭环：

1. 启动 `pre-public runtime`
2. 启动 `AgentInbox`
3. 给 agent 一个订阅任务
4. agent 自己创建 callback 和 subscription
5. 用 `agentinbox fixture emit` 注入事件
6. 观察 `pre-public runtime` 被唤醒
7. 观察 agent 用 `agentinbox inbox list/read` 读回消息

## 一、推荐目录和前提

推荐使用两个独立终端窗口：

- 窗口 A：`pre-public runtime`
- 窗口 B：`AgentInbox`

当前仓库路径：

- `pre-public runtime`: `~/opensource/src/github.com/holon-run/runtime-incubation`
- `AgentInbox`: `~/opensource/src/github.com/holon-run/agentinbox`

建议使用独立的 home 目录，避免污染已有本地状态：

- `PRE-PUBLIC RUNTIME_HOME=/tmp/runtime-incubation-agentinbox-demo`
- `AGENTINBOX_HOME=/tmp/agentinbox-demo`

## 二、启动 pre-public runtime

### 1. 推荐环境变量

```bash
export PRE-PUBLIC RUNTIME_HOME=/tmp/runtime-incubation-agentinbox-demo
export PRE-PUBLIC RUNTIME_HTTP_ADDR=127.0.0.1:7878
export PRE-PUBLIC RUNTIME_CALLBACK_BASE_URL=http://127.0.0.1:7878
export PRE-PUBLIC RUNTIME_CONTROL_AUTH_MODE=disabled
```

说明：

- 第一轮联调先在 loopback 上跑
- 暂时关闭 control token，避免先在认证上分神
- callback base URL 固定成当前本地地址，方便 `AgentInbox` 直接命中

### 2. 启动命令

```bash
cd ~/opensource/src/github.com/holon-run/runtime-incubation
cargo run -- serve
```

### 3. 启动后确认

至少确认这两个点：

- `${PRE-PUBLIC RUNTIME_HOME}` 已创建
- `pre-public runtime` 在 `127.0.0.1:7878` 上可达

如果想手工确认 control plane，可用：

```bash
curl -sS -X POST http://127.0.0.1:7878/control/agents/default/wake \
  -H 'content-type: application/json' \
  -d '{"reason":"manual_probe"}'
```

## 三、启动 AgentInbox

### 1. 推荐环境变量

```bash
export AGENTINBOX_HOME=/tmp/agentinbox-demo
```

### 2. 构建并启动

```bash
cd ~/opensource/src/github.com/holon-run/agentinbox
npm run build
node dist/src/cli.js serve --home "$AGENTINBOX_HOME"
```

默认会使用：

- socket：`${AGENTINBOX_HOME}/agentinbox.sock`
- state：`${AGENTINBOX_HOME}/agentinbox.sqlite`

### 3. 启动后确认

```bash
cd ~/opensource/src/github.com/holon-run/agentinbox
node dist/src/cli.js status --home "$AGENTINBOX_HOME"
```

预期：

- 返回 `ok`
- 能看到当前 home/state/socket 信息

## 四、建议给 agent 的 operator prompt

第一轮不要把任务写得太开放。

建议 prompt：

```text
Use AgentInbox to create a fixture subscription for this agent.

Requirements:
- Create a pre-public runtime callback with delivery_mode=wake_only.
- Register that callback as the activation target in AgentInbox.
- Match only events whose metadata contains {"channel":"engineering"}.
- Use the AgentInbox CLI at:
  /Users/jolestar/opensource/src/github.com/holon-run/agentinbox/dist/src/cli.js
- Pass --home /tmp/agentinbox-demo to every AgentInbox CLI command.
- After you are woken by AgentInbox, do not stop at the wake signal.
- Use agentinbox inbox list and agentinbox inbox read to fetch the inbox item and summarize it.
```

如果需要更硬一点，可以再补一条：

- Do not invent APIs. Use only the existing AgentInbox CLI commands.

## 五、期望 agent 执行的关键动作

不要求字面命令完全一致，但期望路径应接近这样。

### 1. 创建 fixture source

```bash
cd ~/opensource/src/github.com/holon-run/agentinbox
node dist/src/cli.js source add fixture demo --home /tmp/agentinbox-demo
```

### 2. 创建 pre-public runtime wake callback

这里由 agent 调 `CreateCallback`，期望结果包含：

- `delivery_mode = wake_only`
- `callback_url` 形如：
  - `http://127.0.0.1:7878/callbacks/wake/<token>`

### 3. 创建 subscription

期望接近：

```bash
cd ~/opensource/src/github.com/holon-run/agentinbox
node dist/src/cli.js subscription add default <source_id> \
  --home /tmp/agentinbox-demo \
  --match-json '{"channel":"engineering"}' \
  --activation-target 'http://127.0.0.1:7878/callbacks/wake/<token>'
```

如果当前 branch 还支持显式 activation mode，也可以接受：

```bash
--activation-mode activation_only
```

### 4. 被唤醒后读取 inbox

期望接近：

```bash
cd ~/opensource/src/github.com/holon-run/agentinbox
node dist/src/cli.js inbox list --home /tmp/agentinbox-demo
node dist/src/cli.js inbox read <inbox_id> --home /tmp/agentinbox-demo
```

## 六、测试驱动的注入命令

agent 完成订阅后，由测试驱动手工注入 fixture 事件。

### 1. 注入事件

```bash
cd ~/opensource/src/github.com/holon-run/agentinbox
node dist/src/cli.js fixture emit <source_id> \
  --home /tmp/agentinbox-demo \
  --metadata-json '{"channel":"engineering","kind":"review"}' \
  --payload-json '{"text":"PR #123 received a new review: LGTM with one nit"}'
```

### 2. 如果需要，显式 poll subscription

```bash
cd ~/opensource/src/github.com/holon-run/agentinbox
node dist/src/cli.js subscription poll <subscription_id> --home /tmp/agentinbox-demo
```

## 七、联调时的核心观测点

### 1. AgentInbox 侧

应确认：

- `source add` 成功
- `subscription add` 成功
- `fixture emit` 成功
- `subscription poll` 后 inbox item 已 materialize

可以直接检查：

```bash
cd ~/opensource/src/github.com/holon-run/agentinbox
node dist/src/cli.js inbox list --home /tmp/agentinbox-demo
```

### 2. pre-public runtime 侧

应确认：

- callback 被命中
- agent 被唤醒
- wake-only 没有错误变成普通 `CallbackEvent`

如果需要人工辅助观察，可以看 `pre-public runtime` 服务日志。

### 3. Agent 行为侧

最关键的是看 agent 是否真的做了这两件事：

- 订阅是它自己建的
- 唤醒后它真的去读了 inbox

如果它只停留在“我被唤醒了”，那这轮不算通过。

## 八、第一轮通过标准

满足下面 5 条即可认为第一轮联调通过：

1. agent 自己成功创建 `wake_only` callback
2. agent 自己成功创建 `AgentInbox` subscription
3. `fixture emit` 能触发 `AgentInbox -> pre-public runtime` callback
4. `pre-public runtime` 能正确唤醒 agent
5. agent 被唤醒后用 `inbox list/read` 读回真实 item，并给出正确总结

## 九、常见失败点

### 1. agent 发明了不存在的 AgentInbox API

处理：

- 强化 prompt：必须使用现有 CLI
- 显式给出 CLI 路径

### 2. callback URL 注册错了

处理：

- 确认 agent 使用的是 `wake` callback，不是 `enqueue`
- 确认没有复制错 token

### 3. fixture emit 了，但 subscription 没 materialize

处理：

- 检查 `match-json` 是否匹配
- 必要时显式跑 `subscription poll`

### 4. agent 被唤醒了，但没有继续读 inbox

处理：

- prompt 里明确写“wake 后必须用 inbox list/read”
- 如果仍不稳定，下一轮可加更窄的 acceptance check

## 十、第二轮扩展

第一轮跑通后，下一步可以扩展成：

1. `enqueue_message` callback
2. `activation_with_items`
3. GitHub source 替代 fixture source
4. agent 读完 item 后，通过 `deliver send` 回发结果
5. restart 后 callback 和 inbox 状态仍成立
