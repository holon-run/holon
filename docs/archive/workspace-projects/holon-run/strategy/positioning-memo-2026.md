# holon-run 内部定位 Memo（2026）

这份文档不是对外文案。

它的作用是把 `holon-run` 这条线内部真正该怎么理解，怎么取舍，怎么组织，先写死。

## 结论先行

`holon-run` 最该押的，不是 “agent runtime” 本身。

最该押的是：

`local-first agent connectivity stack`

更具体一点：

`让 agent 在本地优先环境里，稳定、安全、可组合地调用工具和服务。`

这才是长期主线。

## 一、为什么不是把 runtime 放在战略中心

`holon` 很重要。

但如果把 `holon` 理解成战略中心，这条线很容易滑向：

- 又一个 agent runner
- 又一个 coding workflow 工具
- 又一个围绕任务执行包装起来的 agent 产品

这个方向不是没价值。

但问题是：

- 太容易和大量现有 agent 产品打成一类
- 辨识度不够强
- 很难把你的独特优势讲清楚

你真正独特的地方，不只是“能跑 agent”。

而是：

- 能统一接各种工具和协议
- 能把浏览器里的能力和登录态接进来
- 能让 agent 在 local-first 环境里真正可执行

这套东西比单纯 runtime 更有位置。

## 二、真正的战略中心是什么

### 1. `uxc`

这是最接近战略中心的资产。

它解决的是：

- 不同协议怎么统一发现和调用
- agent 怎么不为每个服务单独写一套 wrapper
- tool connectivity 怎么从一堆散乱集成变成一层稳定能力

如果未来外部人只能记住 `holon-run` 下面一个基础设施名字，最有机会的是 `uxc`。

### 2. `webmcp-bridge`

这是最有差异化潜力的扩展层。

它的价值不是“又一个 bridge”，而是：

- 浏览器能力接入
- 登录态复用
- human + agent 在同一页面协作
- native WebMCP 和 fallback adapter 的统一通路

这块很像杀手级补充。

它把很多纯 server-side tool stack 做不到的事情带进来了。

### 3. `holon`

`holon` 应该被理解成：

- 旗舰入口
- showcase
- workflow product shell

不是战略中心本身，而是把战略中心消费起来的产品层。

它的作用是证明：

- 这不是一层抽象能力
- 这套东西可以被真实 agent workflow 使用
- 最终能形成产品体验

## 三、推荐的内部结构

### 战略中心

- `uxc`
- `webmcp-bridge`

### 旗舰产品入口

- `holon`

### 新探索方向

- `holon-host`

### 技术支撑层

- `agentvm`

### 已降权 / 停止推进

- `holonbase`
- `agent-account-bridge`

## 四、这意味着什么

这意味着后面所有方向判断，都应该先问：

`它是在增强 connectivity stack，还是在制造新的并列叙事？`

如果是前者，可以继续。

如果是后者，要非常谨慎。

## 五、对外叙事应该怎么收

现在最容易出问题的地方，是外部人看这条线时会同时看到：

- runtime
- MCP
- WebMCP
- bridge
- host
- local-first
- skills
- schema

每个词都对。

但全都同时出现时，等于没有中心。

所以内部必须先统一：

### 公司级一句话

`holon-run` 在做一套面向 local-first workflow 的 agent connectivity stack。

### 三层关系

- `uxc`：统一工具接入层
- `webmcp-bridge`：浏览器和登录态桥接层
- `holon`：把这些能力变成真实工作流入口

### `holon-host`

先作为第二幕候选。

不是现在的 headline。

## 六、未来 12 个月理想状态

### 当前状态

- 你自己知道主线
- 仓库和 repo map 也基本整理出来了
- 但外部人不一定一眼看懂谁是核心，谁是支撑

### 理想状态

- 外部人知道 `uxc` 是统一工具接入层
- 外部人知道 `webmcp-bridge` 是浏览器能力和登录态接入层
- 外部人知道 `holon` 是这套东西跑起来的旗舰入口
- 外部人不会再把 `holon-run` 理解成一堆并列 repo

## 七、现在最该做的事

### 1. 统一一句话定位

把这句话统一写回：

- `projects/holon-run`
- `holon`
- `uxc`
- `webmcp-bridge`

### 2. 明确 repo 角色

不要再让 `holon` 和 `uxc` 在战略层并列竞争。

更准确的关系是：

- `uxc` 是核心基础设施
- `webmcp-bridge` 是高差异化扩展
- `holon` 是旗舰消费端

### 3. 做一个旗舰闭环

不是十个 demo。

一个最能体现这套结构价值的闭环。

最好能同时体现：

- local-first
- 多协议工具调用
- 浏览器登录态复用
- agent 真执行完成任务

## 八、不该做什么

- 不该继续平铺更多并列 side project
- 不该过早把 `holon-host` 抬成主舞台
- 不该再给已降权方向保留过高认知位置
- 不该让 repo 数量代替产品聚焦

## 九、一句话判断

`holon-run` 不是要成为“什么都做一点的 agent 工具集合”，而是要成为一套以 `uxc` 为核心、以 `webmcp-bridge` 为差异化、以 `holon` 为旗舰入口的 local-first agent connectivity stack。
