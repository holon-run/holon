# AgentInbox

`projects/holon-run/agentinbox/` 保存 `AgentInbox` 相关的定位、边界、路线和后续实现判断。

## 基本信息

- `Status`: `new`
- `定位`: local agent ingress and delivery layer
- `角色`: shared subscription source + outbound delivery for local agents
- `GitHub repo`: `https://github.com/holon-run/agentinbox`
- `Local path`: `/Users/jolestar/opensource/src/github.com/holon-run/agentinbox`

## 一句话

`AgentInbox` 是本地 agent 的共享订阅与投递层。

它负责把外部系统的消息流和事件流接到本地，并在需要时激活 `pre-public runtime` 或其他 agent；
同时负责把 agent 的回复、提问、进展更新投递回外部系统。

## 当前文档

- `runtime-incubation-agentinbox-integration-test-plan-2026-04.md`
  - `pre-public runtime` 和 `AgentInbox` 当前联调方案
  - 重点覆盖 callback wake、agent 自主订阅、以及 `inbox list/read` 闭环
- `runtime-incubation-agentinbox-integration-runbook-2026-04.md`
  - 第一轮联调的实际执行手册
  - 包含环境变量、启动命令、operator prompt 和观测点
- `agentinbox-cli-quickstart-for-runtime-incubation-2026-04.md`
  - 专门给 `pre-public runtime` agent 用的最小 CLI 操作说明
  - 只覆盖 source、subscription、inbox、fixture 四组命令

## 当前定位

`AgentInbox` 不是 agent runtime，也不是最终用户产品面。

它更适合被理解成：

- 外部系统到本地 agent 的 ingress layer
- 多 agent 共享的 subscription source 托管层
- outbound message / update delivery 层

## 应负责的事情

- 托管 `GitHub`、`IM`、`MCP`、workspace watcher、web/app bridge 等订阅源
- 让多个 agent 复用同一个订阅源
- 在订阅源之上维护 agent-specific `Interest`
- 产生 activation signal，唤醒目标 agent
- 提供 mailbox / message read 接口
- 负责把 agent 的 reply / ask / update / notify 发回外部系统

## 不应负责的事情

- 不做 long-lived runtime
- 不做 agent queue / wake / sleep / task 语义
- 不做重 workflow engine
- 不做最终用户主产品 shell
- 不在核心层内置完整 connector 业务逻辑到 `pre-public runtime`

## 与其他项目的关系

- `pre-public runtime`
  - 负责 agent runtime、continuity、task、brief、execution
- `uxc`
  - 提供能力接入与调用基础设施
- `webmcp-bridge`
  - 提供 browser / web app 侧 bridge 能力
- `holon`
  - 未来作为默认装配与 operator shell 消费这层能力

## 核心边界

建议保持下面这条边界：

- `AgentInbox` owns sources and delivery
- `pre-public runtime` owns runtime meaning

换句话说：

- `AgentInbox` 负责接世界、托管订阅、读写外部通道
- `pre-public runtime` 负责把收到的东西变成 runtime queue / wake / task 语义

## 当前建议的数据分层

- `SubscriptionSource`
  - 由 `AgentInbox` 托管的共享订阅源
- `Interest`
  - 某个 agent 在某个订阅源上的过滤条件和投递规则
- `InboxItem`
  - 标准化后的消息/事件项
- `Activation`
  - 用于唤醒 agent 的轻量信号

## 当前建议的最小闭环

1. agent 向 `AgentInbox` 注册 `Interest`
2. `AgentInbox` 托管对应 `SubscriptionSource`
3. 有新事件时，`AgentInbox` 产生 `Activation`
4. agent 被激活后读取 `InboxItem`
5. agent 处理任务后，通过 `AgentInbox` 把结果回发到外部系统

## 语言建议

第一版更适合用 `TypeScript / Node.js`。

理由：

- 这一层是 connector / SDK / CLI / WebMCP 密集层
- 第三方集成生态更偏 `TypeScript`
- 更适合快速试验不同 source 和 delivery adapter
- `pre-public runtime` 作为 runtime 核心继续保持在 `Rust` 更合理

后续如果这层被证明需要更强的本地守护进程能力或更重的长期状态管理，再考虑局部下沉到 `Rust`。

## 当前原则

- 组件独立可用
- 对外先围绕真实工作流验证，不先做大而全平台
- 先把 ingress / delivery 跑通，再逐步抽象更强的 interest / routing model
