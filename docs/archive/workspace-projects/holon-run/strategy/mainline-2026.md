# holon-run 主线说明（2026）

## 一句话版本

`holon-run` 这条线要解决的问题是：

`如何让 agent 在 local-first 环境里，稳定、安全、可组合地调用工具和服务。`

## 不是什么

它不应该再被理解成：

- 又一个通用 AI 应用
- 一堆并列的 agent side project
- 看到一个协议或平台就接一个的新工具集合

如果继续按这个方式扩张，很容易积累很多 demo，但很难形成真正的位置。

## 更准确的理解

这条线更像一个面向 agent 的工具基础设施组合，重点是：

- agent 怎么发现工具
- agent 怎么连接远程或本地能力
- agent 怎么在本地优先环境里稳定执行
- 调用链路怎么被复用、治理和组合

## 主问题

把所有仓库收束后，最值得长期押注的主问题是：

`给 agent 提供一个统一的工具接入与执行层，让它们不必围绕每个网站、每个协议、每种接口各写一套独立集成。`

## 仓库分层

### 1. 主仓库

这些仓库最接近主线核心。

#### `uxc`

角色：

- 统一不同接口形态的调用入口
- 收口 OpenAPI、MCP、GraphQL、gRPC、JSON-RPC 等远程能力
- 让 agent 以更统一的方式使用工具

为什么重要：

- 它最接近“agent 工具接入层”这个核心问题

#### `holon`

角色：

- 面向 agent 的主工具运行与使用体验
- 承接 skills、工作流、runtime、上下文和分发方式

为什么重要：

- 它决定这条线最终是一个“技术组件”，还是一个能被真实 agent 工作流使用的产品

### 2. 辅助仓库

这些仓库支撑主线，但不应该抢走主叙事。

#### `webmcp-bridge`

角色：

- 处理网页能力和本地 agent 工具链之间的桥接

补充判断：

- 原来的 `agent-account-bridge` 方向已经并入这里
- 后续不再把两者看成两条独立产品线

#### `holon-host`

角色：

- 新的探索方向，关注 host control plane / app-host 关系

为什么现在重要：

- 它是值得继续观察的新方向
- 如果方向成立，它可能补上 agent 工具层之上的 host 协同问题

#### `agentvm`

角色：

- 探索 agent 执行环境和隔离能力

### 3. 实验仓库

这类仓库更适合被视为试验分支，而不是长期主资产：

- `uxc_*`
- `webmcp-bridge_*`
- `holon-host`
- `agent-account-bridge`
- `holonbase`
- `holon_worktrees`
- 针对单 issue、单集成、单验证场景起的临时仓库

原则是：

- 能合回主线的尽快合回
- 不能合回的尽快归档或清理

## 当前最需要收束的地方

### 1. 主叙事

现在最大的风险不是没东西做，而是能讲出很多条线：

- MCP
- WebMCP
- bridge
- local-first
- runtime
- skills
- schema
- tool service

这些都对，但不能同时做主叙事。

当前最适合做主叙事的是：

`agent tool connectivity and execution for local-first workflows`

### 2. 产品入口

需要尽快明确：

- 外部人首先该理解 `holon` 还是 `uxc`
- 哪个是产品入口
- 哪个是底层能力

当前更合理的关系是：

- `holon`：面向用户和 agent 工作流的入口
- `uxc`：底层能力接入层
- `webmcp-bridge`：特定场景桥接能力

### 3. 做与不做标准

以后一个新方向是否值得继续做，先问三个问题：

1. 它是否强化了“agent 工具接入与执行层”这条主线
2. 它是否能被多个工作流复用，而不是一次性 demo
3. 它是否应该进入主仓库，而不是停留在实验仓库

只要连续两个回答是否定，就不应该继续扩张。

## 未来 90 天建议

### 第一件事

明确对外一句话定位，并统一写回 `holon`、`uxc`、`workspace/projects/holon-run`

### 第二件事

给现有仓库做一次分类：

- 主仓库
- 辅助仓库
- 实验仓库
- 可归档仓库

其中当前应明确：

- `agent-account-bridge`：已废弃，方向并入 `webmcp-bridge`
- `holonbase`：暂时放弃，归入可归档仓库
- `holon-host`：新探索方向，先按实验型辅助仓库管理

### 第三件事

减少零散新实验，优先补：

- 主链路文档
- 核心 demo
- 一个能体现主线的真实使用闭环

## 一句话判断

`holon-run` 不是要把所有 agent 工具都做一遍，而是要成为 agent 在 local-first 世界里连接和执行工具的那一层。
