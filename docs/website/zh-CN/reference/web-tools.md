---
title: Web 工具
summary: WebFetch 和 WebSearch 的参数、提取模式、搜索提供商、截断和来源处理。
order: 42
---

# Web 工具

Holon Agent 内置两个网络工具，用于检索和搜索公开网络。它们属于 `Web` 能力族，默认对
每个 Agent 可用。

## WebFetch

`WebFetch` 抓取指定的 HTTP 或 HTTPS URL，提取可读文本，并返回结构化的来源信息。Agent
用它读取网页、文档或 API 响应。

### 参数

| 参数 | 必填 | 说明 |
|-----------|----------|-------------|
| `url` | 是 | 要抓取的 HTTP 或 HTTPS URL |
| `max_chars` | 否 | 返回的最大字符数（默认：无硬性上限） |
| `extract_mode` | 否 | 如何从响应中提取内容 |

### 提取模式

| 模式 | 行为 |
|------|----------|
| `auto`（默认） | 检测内容类型：把 HTML 渲染为文本，文本原样传递 |
| `text` | 剥离 HTML 标签，返回纯文本 |
| `raw` | 返回未处理的原始响应体 |

### WebFetch 返回什么

每次响应都带着来源元数据：

- 最终 URL（重定向之后）
- HTTP 状态码
- 内容类型
- 截断标志和字符数
- 内容哈希（SHA-256）

抓取到的内容被运行时视为**不可信的外部内容**。Agent 收到的是带来源包装的内容，并被要求
不能仅凭抓取内容就提升信任级别。

### 用法示例

Agent 像调用其他工具一样调用 WebFetch：

```
WebFetch { url: "https://example.com/docs/api", max_chars: 5000 }
```

运行时抓取该 URL，应用 Holon 的网络策略，提取可读文本，并返回结果。

## WebSearch

`WebSearch` 通过 Holon 的网络提供商注册表搜索网络，返回带引用的结构化结果。

### 参数

| 参数 | 必填 | 说明 |
|-----------|----------|-------------|
| `query` | 是 | 搜索查询字符串 |
| `max_results` | 否 | 返回的最大结果数 |
| `provider` | 否 | 使用的搜索提供商（默认：已配置的提供商） |

### 搜索提供商

Holon 使用基于提供商的搜索模型，有多种提供商可选：

- **DuckDuckGo（托管）**：Holon 内置的托管搜索提供商，不需要 API key。在引导流程中选择
  "Managed WebSearch: DuckDuckGo" 或 "Auto" 模式即可启用。
- **腾讯云 WSA**：腾讯云 SearchPro / Web Search API。需要把 API key 存为凭据 profile。
  配置方式：`holon config set web.providers.tencent.kind tencent_cloud_wsa` 和
  `holon config set web.providers.tencent.credential_profile <profile>`。
- **博查 AI 搜索**：博查 AI Web Search API。需要把 API key 存为凭据 profile。配置方式：
  `holon config set web.providers.bocha.kind bocha` 和
  `holon config set web.providers.bocha.credential_profile <profile>`。
- **模型原生搜索**：部分模型提供商（OpenAI、Anthropic）通过自家 API 支持原生网络搜索。
  在 "Auto" 模式下，Holon 会优先使用它们。

所有提供商返回的搜索结果都会标准化为一致格式，包含标题、URL 和摘要文本。引用会被保留，
以便 Agent 用 `WebFetch` 跟进读取完整页面。

搜索配置属于引导流程的一部分，之后可用 `holon config set` 修改。

### WebSearch 返回什么

每条结果包含：

- 标题和 URL
- 摘要或概述文本
- 来源归属

结果是结构化的，Agent 可以在需要时用 `WebFetch` 跟进读取完整页面。工具描述明确告诉
Agent："Use WebFetch after search when full page content is needed."

### 用法示例

```
WebSearch { query: "Rust async runtime design patterns", max_results: 5 }
```

## 网络策略

Holon 对所有抓取和搜索操作应用可配置的网络策略：

- **允许的协议**：仅 `http` 和 `https`
- **域名过滤**：可配置的允许/拒绝列表
- **超时**：可配置的单请求超时
- **重定向**：跟随到可配置的上限

策略通过 Holon 配置控制，统一作用于 WebFetch 和 WebSearch。

## Agent 何时使用这些工具

Agent 根据任务上下文决定何时使用网络工具。常见模式：

- **调研**：Agent 用 WebSearch 找到信息，再用 WebFetch 读取具体页面。
- **查文档**：Agent 从网络上抓取 API 文档、RFC 或包文档。
- **验证**：Agent 用公开来源交叉核对说法。

运行时在面向模型的工具 schema 中暴露这两个工具，Agent 在任务需要网络访问时通过正常的
工具调用选择它们。

## XSearch

`XSearch` 使用 xAI 托管的 `x_search` 端点搜索公开的 X（Twitter）帖子。它作为一次隔离的
提供商请求运行，独立于主对话模型，并返回带引用的持久文本。

### 何时使用 XSearch

用于 X 特有的内容、账号或讨论。一般网页搜索用 `WebSearch`。

### 参数

| 参数 | 必填 | 说明 |
|-----------|----------|-------------|
| `query` | 是 | 搜索查询字符串 |
| `allowed_x_handles` | 否 | 只保留这些 X 账号的结果（最多 10 个，不带 `@`） |
| `excluded_x_handles` | 否 | 排除这些 X 账号的结果（最多 10 个，不带 `@`） |
| `from_date` | 否 | 起始日期，`YYYY-MM-DD` 格式 |
| `to_date` | 否 | 结束日期，`YYYY-MM-DD` 格式 |

### XSearch 返回什么

| 字段 | 说明 |
|-------|-------------|
| `text` | 模型响应中的搜索结果文本 |
| `citations` | 结构化引用，含 URL、标题和文本位置索引 |
| `provider` | 始终为 `xai` |
| `backend` | 始终为 `x_search` |
| `model` | 用于搜索的 xAI 模型 |
| `diagnostics` | 提供商请求 ID、延迟和托管条目类型计数 |

### 前置条件

XSearch 需要：

1. **配置好 xAI 提供商**，使用 `openai_responses` transport 和有效凭据（通过 Codex 的
   OAuth 设备登录，或 API key）。
2. **启用 XSearch**（有 xAI 凭据时默认启用）。

停用 XSearch：

```bash
holon config set x_search.enabled false
```

### 配置

XSearch 配置使用这些键：

| 键 | 类型 | 默认值 | 说明 |
|-----|------|---------|-------------|
| `x_search.enabled` | boolean | `true` | 有 xAI 凭据时启用 XSearch |
| `x_search.model` | model_ref | `grok-4.3` | 隔离 XSearch 请求使用的 xAI 模型路由 |
| `x_search.timeout_seconds` | integer | `60` | 请求超时秒数 |

默认模型是 `grok-4.3`。XSearch 使用 xAI 提供商的 OAuth 凭据，只在收到 401 Unauthorized
响应时刷新 token。

### 用法示例

```
XSearch { query: "Holon runtime agent framework", from_date: "2026-01-01" }
```

## 配置

网络工具通过 Holon 的 web 配置项控制：

```bash
# 查看当前 web 配置
holon config get web.search.enabled

# 完全停用网络工具
holon config set web.fetch.enabled false
holon config set web.search.enabled false
```

完整的 web 配置 schema 见[配置参考](/zh-CN/reference/configuration.md)。

## 另见

- [模型工具 schema 清册](/zh-CN/reference/model-tool-schema-inventory.md)：工具注册与稳定性
- [集成指南](/zh-CN/guides/automate-over-http.md)：HTTP 控制平面和 webhook
- [配置参考](/zh-CN/reference/configuration.md)：网络策略设置
