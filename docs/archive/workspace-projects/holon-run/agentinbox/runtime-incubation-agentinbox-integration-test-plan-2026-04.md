# pre-public runtime / AgentInbox 联调测试方案（2026-04）

这份文档记录当前 `AgentInbox -> pre-public runtime` 订阅与通知机制的第一轮联调方案。

目标不是一次性把完整产品工作流都测完，而是先跑通最关键的闭环：

1. agent 自己创建 `pre-public runtime` callback capability
2. agent 自己通过 `agentinbox` CLI 注册订阅
3. `AgentInbox` 产生 activation
4. `AgentInbox` 通过 callback 唤醒 `pre-public runtime`
5. agent 被唤醒后，再通过 `agentinbox` CLI 读取 inbox item
6. agent 输出正确处理结果

## 一、为什么这样测

这套方案对应当前的边界约定：

- `AgentInbox` owns sources and delivery
- `pre-public runtime` owns runtime meaning

换句话说：

- `AgentInbox` 负责 source、subscription、inbox item、activation
- `pre-public runtime` 负责 callback ingress、wake、activation context、agent runtime
- agent 自己决定什么时候去读 inbox item

所以联调的主线不应该是“`AgentInbox` 直接把完整业务消息推给 `pre-public runtime`”，而应该是：

- `AgentInbox` 先触发 wake
- agent 再主动读消息

## 二、测试范围

第一轮只测一个最小真实场景：

- source: `fixture`
- delivery mode: `wake_only`
- inbox reader: `agentinbox inbox list/read`
- event type: 一条简单的工程消息

第一轮暂时不测：

- `enqueue_message` callback
- GitHub/Feishu 等真实 connector
- agent 处理后再通过 `AgentInbox deliver send` 回发外部系统
- 多 agent 共享同一个 source
- restart/recovery

## 三、现有接口基线

### pre-public runtime

当前与联调直接相关的接口：

- control plane
  - `POST /control/agents/:agent_id/prompt`
- callback plane
  - `POST /callbacks/wake/:token`
  - `POST /callbacks/enqueue/:token`
- agent tool
  - `CreateCallback`

当前 wake callback 的预期语义：

- 不产生普通 `CallbackEvent`
- 会触发 wake
- 当前 turn 的 context 会带 `activation_context`

### AgentInbox

当前仓库里已经有可用 CLI：

- `agentinbox source add fixture <source_key>`
- `agentinbox subscription add <agent_id> <source_id> ...`
- `agentinbox subscription poll <subscription_id>`
- `agentinbox inbox list`
- `agentinbox inbox read <inbox_id>`
- `agentinbox fixture emit <source_id> ...`
- `agentinbox status`

当前 CLI 说明可见：

- `~/opensource/src/github.com/holon-run/agentinbox/src/cli.ts`
- `~/opensource/src/github.com/holon-run/agentinbox/README.md`

## 四、测试拓扑

第一轮建议使用 3 个真实组件：

1. 一个真实 `runtime-incubation serve`
2. 一个真实 `agentinbox serve`
3. 一个测试 driver

这里的测试 driver 可以先是人工操作加少量 shell 脚本，不要求一开始就做成全自动集成测试。

原因：

- 现在更重要的是看清楚接口和行为是否顺
- 不是先追求测试框架完整度

## 五、推荐执行步骤

### 阶段 A：基础 wiring

先确认两个服务都能独立工作。

#### 1. 启动 `pre-public runtime`

示例：

```bash
cd ~/opensource/src/github.com/holon-run/runtime-incubation
cargo run -- serve
```

建议记录：

- `PRE-PUBLIC RUNTIME_HOME`
- control base URL
- callback base URL

#### 2. 启动 `AgentInbox`

示例：

```bash
cd ~/opensource/src/github.com/holon-run/agentinbox
npm run build
node dist/src/cli.js serve
```

建议记录：

- `AGENTINBOX_HOME`
- socket path 或 URL

#### 3. 确认 `AgentInbox` 可访问

示例：

```bash
node dist/src/cli.js status
```

预期：

- 返回 `ok`
- 能看到当前 home/db/socket 信息

### 阶段 B：agent 自己建立订阅

这一阶段才开始让 agent 参与。

#### 1. operator 给 agent 一个明确任务

建议 prompt 目标：

- 为 `fixture` source 创建一个订阅
- 生成 `wake_only` callback
- 把 callback URL 注册到 `AgentInbox`
- 当收到 wake 后，再读取 inbox item 并总结内容

这里要明确告诉 agent：

- 必须用 `CreateCallback`
- 必须用 `agentinbox` CLI
- wake 之后必须调用 `agentinbox inbox list` 和 `agentinbox inbox read`

#### 2. agent 应该执行的 CLI 路径

建议期望的命令序列接近这样：

```bash
node dist/src/cli.js source add fixture demo
node dist/src/cli.js subscription add <agent_id> <source_id> \
  --activation-target <runtime-incubation_wake_callback_url> \
  --activation-mode activation_only \
  --match-json '{"channel":"engineering"}'
```

其中：

- `source_id` 可以由 `source add` 返回
- `activation-target` 使用 `pre-public runtime` 的 `wake` callback URL

#### 3. 阶段 B 的验收标准

- agent 自己成功创建 callback
- agent 自己成功创建 subscription
- `AgentInbox` 里出现对应 inbox / subscription 记录
- callback URL 不是测试 driver 代写进去的，而是 agent 真正生成并注册的

### 阶段 C：事件注入与处理

#### 1. 测试 driver 注入 fixture 事件

建议 payload：

```bash
node dist/src/cli.js fixture emit <source_id> \
  --metadata-json '{"channel":"engineering","kind":"review"}' \
  --payload-json '{"text":"PR #123 received a new review: LGTM with one nit"}'
```

#### 2. 推动 subscription materialization

如果当前 `AgentInbox` 还需要显式 poll：

```bash
node dist/src/cli.js subscription poll <subscription_id>
```

#### 3. 观察 `pre-public runtime` 被唤醒

预期：

- `AgentInbox` 成功调用 `pre-public runtime` callback
- `pre-public runtime` 当前 turn 获得 `activation_context`
- 不产生普通 `CallbackEvent`

#### 4. 观察 agent 后续动作

预期 agent 会继续调用：

```bash
node dist/src/cli.js inbox list
node dist/src/cli.js inbox read <inbox_id>
```

然后给出最终处理结果。

#### 5. 阶段 C 的验收标准

- wake 发生
- agent 没停在“我被唤醒了”
- agent 真的去读了 inbox
- 最终输出引用了真实 item 内容

## 六、核心观测点

联调时建议明确看这 4 层。

### 1. AgentInbox 层

- source 是否创建成功
- subscription 是否创建成功
- inbox item 是否 materialize 成功
- activation delivery 是否成功

### 2. pre-public runtime callback 层

- callback 是否命中 `/callbacks/wake/:token`
- 响应 disposition 是：
  - `triggered`
  - 或 `coalesced`

### 3. pre-public runtime runtime / context 层

- 本轮 wake 是否生成 `activation_context`
- `activation_context` 是否至少包含：
  - source
  - resource / subscription / interest 线索
  - callback body 或 body 摘要

### 4. Agent 行为层

- 是否调用了 `CreateCallback`
- 是否调用了 `agentinbox subscription add`
- 是否在 wake 之后调用了 `agentinbox inbox list/read`
- 最终输出是否使用了真实 inbox 内容

## 七、第一轮推荐的 operator prompt

建议用这种风格，不要太开放：

```text
Use AgentInbox to create a fixture subscription for this agent.

Requirements:
- Create a pre-public runtime callback with delivery_mode=wake_only.
- Register that callback as the activation target in AgentInbox.
- Match only events whose metadata contains {"channel":"engineering"}.
- After you are woken by AgentInbox, do not stop at the wake signal.
- Use agentinbox inbox list and agentinbox inbox read to fetch the inbox item and summarize it.
```

如果要进一步降低跑偏概率，可以补充：

- `agentinbox` CLI 的绝对路径
- `--home` / `--socket` / `--url` 怎么传

## 八、失败时如何定位

如果这轮联调失败，建议按这个顺序定位：

1. `AgentInbox` subscription 没建成
2. callback URL 没注册成功
3. fixture event 没 materialize 成 inbox item
4. callback 没打到 `pre-public runtime`
5. `pre-public runtime` wake 了，但没有正确暴露 `activation_context`
6. agent 醒了，但没有继续调用 `agentinbox inbox list/read`
7. agent 读了 inbox，但没有正确理解 item 内容

这样可以快速区分问题是在：

- `AgentInbox`
- `pre-public runtime`
- callback contract
- 还是 agent 自己的行为

## 九、第二轮扩展

如果第一轮 `wake_only` 跑通，第二轮建议再补：

1. `enqueue_message`
   - 验证 `AgentInbox` 能直接把 payload 推给 `pre-public runtime`
2. 非 JSON body
   - 验证 body 无感 contract
3. 多条 activation 连续到来
   - 验证 coalescing 行为
4. callback restart recovery
   - 验证 `pre-public runtime` 重启后 token 仍然可用
5. agent ack/read 后的 outbound delivery
   - 验证 `AgentInbox` 的 reply/update 路径

## 十、结论

第一轮最值得验证的不是“所有系统都接上了”，而是下面这条最小闭环：

1. agent 创建 callback
2. agent 创建 `AgentInbox` subscription
3. `AgentInbox fixture emit`
4. `AgentInbox` 唤醒 `pre-public runtime`
5. agent 用 `agentinbox inbox list/read` 读回消息

只要这条闭环跑通，就说明：

- callback contract 是对的
- wake 语义是对的
- `AgentInbox` / `pre-public runtime` 的边界是清楚的
- agent 自主订阅 + 被动唤醒 + 主动取信 这条核心工作流成立
