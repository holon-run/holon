# local-first agent substrate 候选能力图（2026）

这份文档不是在给 `holon-run` 发明一个新的大词。

它只做一件事：

`把 local-first agent 在用户侧持续工作时，可能真正需要的能力拆开，并标注哪些该进主线，哪些先做 probe。`

当前一个明显风险是，把这件事讲成 “local cloud”。

这个说法有直觉吸引力。

但它也很危险，因为它很容易把方向带向：

- 小 Kubernetes
- 本地 service mesh
- 通用调度系统
- 很完整，但离 agent 真正任务闭环很远的平台

所以这里先统一一个更克制的说法：

`local-first agent substrate`

或者更偏产品一点：

`user-side control plane for agents`

## 一、结论先行

当前最合理的切法，不是去做一个“大而全 local cloud”。

更合理的是把候选能力拆成三组：

### 1. 应进入主线的基础能力

- capability access
- session lifecycle
- artifact handling
- event / subscription primitives

### 2. 应保持为边缘延伸或 bridge 的能力

- auth integration
- browser session reuse
- provider-specific adapters

### 3. 当前更适合 probe，而不适合做成底层平台的能力

- visible control surface
- approval / policy UX
- 更直接面向最终用户的 product shell

一句话：

`先围绕 agent 任务闭环长，不要围绕云平台组件表长。`

## 二、怎么判断一种能力值不值得进入主线

不是看它抽象上是否重要。

而是看它是否同时满足下面三条：

1. 多个工作流都会反复碰到
2. 不解决它，agent 在用户侧就很难稳定持续工作
3. 它可以先被定义成最小 contract，而不是要求先做一整套平台

如果三条里只能满足一条或两条，更适合先 probe。

## 三、候选能力分层

## A. capability access

问题：

`agent 怎么碰到真实能力。`

包括：

- OpenAPI
- MCP
- GraphQL
- gRPC
- JSON-RPC
- CLI
- 浏览器页面能力

当前判断：

- 这是主线
- `uxc` 是最核心资产

为什么该进主线：

- 这是所有 local-first agent 工作流的入口问题
- 复用性最强
- 已经有连续资产和外部可识别性

当前对应资产：

- `uxc`
- 部分 `webmcp-bridge`

## B. session lifecycle

问题：

`agent 调到的能力，如何持续、复用、独占、恢复、清理。`

包括：

- session identity
- reuse / exclusive 语义
- idle timeout
- daemon / stdio / browser session 的统一表达

当前判断：

- 这是主线候选
- 但应先做 contract，不急着做大系统

为什么该进主线：

- 本地 agent 很多稳定性问题，本质都不是“不会调用”，而是 session 不稳
- 这块跨工作流复用度高

当前建议：

- 优先在 `uxc` 层定义 contract
- 不要先发明通用 session manager 平台

## C. artifact handling

问题：

`长输出、文件输入、临时对象、结果引用，如何不把上下文炸掉。`

包括：

- file input
- local artifact output
- inline vs reference return
- artifact addressing
- 大结果分页或落地

当前判断：

- 这是主线候选
- 很可能比继续扩更多协议更快提升 agent 可用性

为什么该进主线：

- 直接影响长任务稳定性
- 是 connectivity 和 execution 之间最实际的一层
- 很多 agent 产品会在这里失真

当前建议：

- 在 `uxc` 里逐步把 artifact contract 抽出来
- `webmcp-bridge` 需要跟进 browser-side artifact 输出

## D. event / subscription primitives

问题：

`agent 不只要主动 call，还要能等事件、处理持续流入的变化。`

包括：

- subscriptions
- watches
- webhooks / local events 的统一消费
- 长任务中的状态推进

当前判断：

- 这是主线候选
- 但暂时比 session / artifact 更靠后

为什么该进主线：

- 这关系到 agent 从一次性助手，走向持续工作体
- 对 local-first 很关键，因为很多用户侧变化天然就是事件流

当前建议：

- 作为 `uxc` runtime contract 的延伸方向
- 先结合真实工作流验证，不急着抽象成完整事件总线

## E. auth integration

问题：

`agent 如何安全获取和使用用户已经拥有的凭证。`

包括：

- API keys
- OAuth tokens
- 1Password / local secret sources
- 网页登录态的引用

当前判断：

- 这是重要能力
- 但不适合被抽象成一个单独的“大 auth layer”

为什么不宜单独升格：

- 一部分已经是 `uxc` 的凭证 contract 问题
- 一部分本质是浏览器会话和 provider 特性问题
- 太早抽象，很容易做成一个过大的通用身份系统

当前建议：

- `uxc` 吃 credential access contract
- `webmcp-bridge` 吃浏览器登录态复用
- provider-specific 的 auth dance 留在 adapter / integration 层

## F. browser session reuse

问题：

`浏览器里已登录的真实用户会话，怎样被本地 agent 安全利用。`

包括：

- profile reuse
- native WebMCP
- fallback adapters
- shared browser surface

当前判断：

- 这是高差异化方向
- 应作为 bridge / edge 能力持续推进

为什么不直接当全部主线：

- 它很重要，但不是全部 local-first substrate
- 更像能力 access 的高价值边缘

当前对应资产：

- `webmcp-bridge`

## G. visible control surface

问题：

`用户怎样看见 agent 在做什么，并在必要时接管、批准、理解结果。`

包括：

- 当前任务状态
- artifact 可见性
- session 可见性
- approval / interrupt / inspect

当前判断：

- 这是 probe 优先，不宜现在做成底层平台

为什么：

- 它很可能重要
- 但它更接近产品形态问题，不是底层最小 contract
- 太早平台化，很容易把系统做重

当前建议：

- 通过 `holon` 或更轻的实验入口做 probe
- 不急着做统一 control panel 平台

## H. policy / approval / audit

问题：

`什么动作能直接做，什么动作要确认，做完能否追踪。`

包括：

- destructive vs read-only
- human approval points
- audit trail
- replay / explainability

当前判断：

- 重要，但暂时不适合作为底层平台主轴

为什么：

- policy 强依赖产品语境
- 不同场景的审批和限制方式差异很大
- 底层更适合先暴露风险元数据，而不是直接决定 policy

当前建议：

- 底层先提供 capability metadata
- 真正的 decision 留给更上层 workflow / product

## I. final product shell

问题：

`这条线最终是否需要一个更直接面向最终用户的产品入口。`

当前判断：

- 很值得探索
- 但应明确归为 probe

为什么：

- 这是当前缺口之一
- 但还没有足够证据说明应该长成什么样
- 过早定型，容易把主线绑死

当前建议：

- 围绕真实场景做产品壳 probe
- 不要现在就把所有能力都塞进一个大而全 app

## 四、当前推荐的主线切法

如果只保留一个最小但连贯的主线结构，我当前更倾向于：

### capability plane

由 `uxc` 为主承接：

- capability access
- session contract
- artifact contract
- 部分 event contract

### browser / session edge

由 `webmcp-bridge` 为主承接：

- browser session reuse
- native WebMCP path
- fallback adapter path
- shared browser surface

### workflow shell

由 `holon` 或后续更清楚的入口承接：

- visible control
- workflow packaging
- user-facing interaction model

这个切法的好处是：

- 不会把一切都塞给 `uxc`
- 不会把 bridge 误当成全部主线
- 不会过早把 visible product shell 平台化

## 五、当前推荐的 probe 列表

如果只选最值得开的几类 probe，我会优先看这三个：

### 1. artifact-first probe

验证问题：

`在本地 agent 长任务里，artifact reference 是否比继续增强 tool call 更值钱。`

为什么值得做：

- 这很可能直接提升稳定性
- 也最容易回流进主线

### 2. session-first probe

验证问题：

`agent 的真正稳定性瓶颈，是否在 session identity / reuse / recovery 语义。`

为什么值得做：

- 这关系到 `uxc` 往下一步怎么长

### 3. visible-control probe

验证问题：

`用户是否需要一个轻量但可见的本地 control surface，来看见、批准、接管 agent 行为。`

为什么值得做：

- 这关系到最终用户产品壳是否必要
- 也关系到 `holon` 的真实定位

## 六、当前明确不该做的事

- 不该先设计一套完整 local cloud 架构图
- 不该先做通用本地调度平台
- 不该先做大而全 policy engine
- 不该把每个新能力都提升成单独产品线
- 不该因为 “都重要” 就把所有层一起推进

## 七、一句话判断

当前最值得做的，不是发明一个复杂的 local cloud。

而是围绕 agent 任务闭环，先把下面这条线做实：

`capability access -> session / artifact substrate -> visible control surface`

其中：

- capability access 是当前主轴
- session / artifact 是最值得延伸的下一层
- visible control 更适合作为 probe 来验证产品形态
