# pre-public runtime / AgentInbox / holon 方向 Memo（2026-04）

日期：`2026-04-04`

这份 memo 用来记录今天关于 `pre-public runtime`、`AgentInbox` 和 `holon` 的战略讨论结论。

重点不是实现计划，而是：

- 这些项目分别解决什么问题
- 它们之间如何分层
- 当前阶段应该怎样推进

## 一、总判断

当前更合理的路线，不是做一个大而全的 `all-in-one` agent 产品。

更合理的路线是：

- 保持组件分层和独立可用
- 用一条真实工作流把这些组件持续拉通
- 最后由 `holon` 做默认装配和产品面

一句话：

`内部模块化，外部任务化。`

## 二、为什么不是 all-in-one

agent 时代的工作流天然跨边界：

- 事件来自一个系统
- 能力来自另一个系统
- 人的确认在第三个系统
- 真正执行发生在本地 runtime

因此底层更适合做成可组合切面，而不是提前收成一个大一统系统。

但这不意味着对外要讲一堆组件。

外部仍然应该感知到一个明确能力，例如：

`把一个任务交给 agent，它会持续跟进外部变化，必要时来问你，然后把任务推进到下一个明确状态。`

## 三、pre-public runtime 的定位

`pre-public runtime` 是 headless、event-driven、long-lived 的 agent runtime。

它负责：

- agent lifecycle
- queue / wake / sleep
- task / brief
- execution continuity
- daemon / host / API surface

它不应负责：

- 大量 provider-specific connector
- 外部系统订阅和消息接入
- 最终用户产品 shell

更准确地说：

`pre-public runtime` 是 runtime product boundary，而不是最终 assembled product boundary。

## 四、AgentInbox 的定位

`AgentInbox` 是本地 agent 的共享订阅与投递层。

它负责：

- 托管外部订阅源
- 让多个 agent 共享同一个 source
- 在 source 上挂 agent-specific `Interest`
- 标准化 inbound item
- 产生 activation signal
- 提供 mailbox / message read 接口
- 负责 outbound delivery

它不负责：

- runtime semantics
- wake/sleep policy
- task orchestration
- prompt / reasoning
- 最终用户产品面

一句话边界：

- `AgentInbox` owns sources and delivery
- `pre-public runtime` owns runtime meaning

## 五、为什么 AgentInbox 需要独立出来

如果没有 `AgentInbox`，`pre-public runtime` 或其他 runtime 很容易逐步吸收：

- GitHub SDK
- IM SDK
- MCP session wiring
- webhook intake
- callback routing
- reply/thread routing
- watcher lifecycle

这样 runtime 会被 connector 重力拖脏。

`AgentInbox` 的存在，是为了把：

- shared subscription source
- activation
- outbound delivery

从 runtime 中抽出来。

## 六、AgentInbox 的内部模型

当前讨论后的更合理切法是：

- `SubscriptionSource`
  - `AgentInbox` 托管的共享订阅源
- `Interest`
  - 绑定在 source 上的过滤条件和投递规则
- `InboxItem`
  - source 产生的标准化消息/事件
- `Activation`
  - 用于唤醒 agent runtime 的轻量信号

关键判断：

- `Interest` 不必先设计成重 DSL
- `SubscriptionSource` 应该能服务多个 agent
- `Interest` 只是 source 上的 agent-specific filter

这意味着 `AgentInbox` 更像共享订阅与投递基础设施，而不是 workflow engine。

## 七、Inbound / Outbound 判断

`AgentInbox` 不应只是“收消息”。

它应该同时负责：

- inbound：接消息、事件、通知、回调，激活 agent
- outbound：把 agent 的 reply / ask / update / notify 发回外部系统

这里不需要过早统一完整消息语义。

更合理的第一阶段原则是：

- 先统一路由边界
- 轻量统一消息类型
- payload 可以先保持 source-specific

一句话：

`先统一 where，轻量统一 why，暂时不强行统一 what。`

## 八、与 uxc / webmcp-bridge 的关系

`AgentInbox` 不应重新实现能力接入层。

应该明确复用：

- `uxc`
  - 作为 CLI / MCP / OpenAPI / GraphQL / JSON-RPC / gRPC 等能力调用的基础层
- `webmcp-bridge`
  - 作为 browser / web-app 场景的 bridge 能力层

分工应保持为：

- `uxc` 负责 capability execution
- `webmcp-bridge` 负责 web edge
- `AgentInbox` 负责 source hosting / activation / delivery
- `pre-public runtime` 负责 runtime continuity

## 九、holon 的定位

`holon` 不应直接等于 `pre-public runtime`。

虽然 `pre-public runtime` 确实吸收了 `holon` 原来 long-lived runtime 的许多方向，
但 `holon` 更适合保留为：

- 总品牌
- 最终装配层
- operator shell / control plane
- 对外的完整产品面

因此更合理的结构是：

- `holon`：assembled product / operator plane
- `pre-public runtime`：runtime core
- `AgentInbox`：ingress and delivery layer

一句话：

`holon` 适合做“整体”，不适合做“某一层”。`

## 十、当前对外第一能力的方向

当前最值得继续压测的，不是抽象的 runtime 叙事，而是一个更具体的用户承诺：

`把一个任务交给 agent，它会持续跟进外部变化，必要时来问你，然后把任务推进到下一个明确状态。`

这里：

- 任务系统是工作对象面
- IM 是持续协作面
- `AgentInbox` 负责消息和事件接入 / 投递
- `pre-public runtime` 负责跨时间持续工作

## 十一、推进方式

当前路线建议是：

1. 先把分层定死
2. 各组件独立可用
3. 让 `pre-public runtime` 和 `AgentInbox` 并行推进
4. 用同一条真实主链路持续拉通
5. 最后由 `holon` 做默认装配

这里的关键不是“先做所有组件，再最后集成”，而是：

`边做组件，边用一条真实工作流持续校验它们能否拼起来。`

## 十二、今天拍定的阶段性结论

- 继续沿 composable slices 路线走，不做 all-in-one 优先
- `pre-public runtime` 保持 runtime 核心定位
- 新建 `AgentInbox`，作为独立 ingress/delivery 层
- `AgentInbox` 当前工作名可先保留
- `holon` 保留为总品牌和最终装配层
- `pre-public runtime` 和 `AgentInbox` 可以并行开发
- `AgentInbox` 第一版更适合用 `TypeScript / Node.js`

## 十三、暂不急着定的事

以下问题今天不需要定死：

- `holon` 的一句话品牌定位
- `Interest` 的完整条件表达能力
- `AgentInbox` 的最终对外品牌名是否保留
- 第一阶段具体 source / connector 范围

这些更适合等实现和真实主链路压测后再继续收敛。
