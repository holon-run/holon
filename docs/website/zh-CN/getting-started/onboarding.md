---
title: 引导配置
summary: 用 `holon onboard` 交互式完成提供商、凭据、模型和搜索配置。
order: 30
---

# 引导配置

`holon onboard` 是配置 Holon 最快的方式。它会启动一个交互式终端 UI 向导，带你完成提供商选择、
凭据录入、模型选择和搜索配置，并把结果写入 Holon 配置和凭据存储。

## 什么时候用引导配置

- **首次安装** — 你刚安装 Holon，需要在创建 Agent 前配置模型提供商。
- **切换提供商** — 你想更换默认提供商或模型，且偏好引导式流程而非手工编辑配置。
- **凭据修复** — 已保存的凭据过期或失效，Holon 检测到需要处理。

## 快速开始

```bash
holon onboard
```

这会启动交互式 TUI。完成后你就拥有可用的默认模型配置——不需要手动编辑配置文件。

## 引导流程

向导共五步。用方向键导航，回车选择，Esc 返回。每一步确认后才进入下一步。

### 1. 选择提供商

从内置列表中选择你的模型提供商：

- **Anthropic** — 通过 Anthropic Messages API 使用 Claude 模型（API key）
- **OpenAI** — 通过 OpenAI API 使用 GPT 和 o 系列模型（API key）
- **OpenAI Codex** — Codex 托管模型（浏览器 OAuth 登录）
- **DeepSeek** — DeepSeek 模型（API key）
- **Gemini** — Google Gemini 模型（API key）
- **自定义提供商** — 任何 OpenAI 兼容端点

向导只显示尚未配置的提供商。如果已经配置了一个提供商并想切换，向导会把它作为更新候选显示。

### 2. 录入凭据

凭据步骤取决于提供商：

- **API key 提供商** — 输入 key。输入不会回显到屏幕，也不会以明文存进配置文件。
- **OpenAI Codex（OAuth）** — 向导打开浏览器完成 OAuth 登录，然后自动捕获凭据。
- **免凭据提供商** — 跳过；直接完成配置，无需认证。

凭据保存在 `~/.holon/credentials.json` 的 Holon 凭据存储中，而不是 `config.json`。
这样密钥不会出现在配置文件和 shell 历史里。该存储是单个加密的 JSON 文件。

### 3. 选择模型

选择默认模型。向导列出该提供商的常用模型并附简短说明。如果列表中没有你想要的，
也可以直接输入自定义模型 ID。

选中的可执行路由会以规范的 `provider@endpoint/model` 形式写入 `model.default`。
之后可以按 Agent 覆盖（`holon agent model set`）；旧的 `provider/model` 写法仍然接受。

### 4. 搜索配置

Holon Agent 可以把 WebSearch 作为内置工具使用。向导提供三种搜索模式：

| 模式 | 行为 |
|------|----------|
| **禁用** | 不提供网页搜索能力 |
| **自动** | 优先使用模型原生搜索（如可用），否则回退到托管 DuckDuckGo |
| **托管（DuckDuckGo）** | 使用 Holon 内置的 DuckDuckGo 搜索提供商 |

搜索配置之后可以用 `holon config set` 修改。

### 5. 确认并应用

应用之前，向导会展示所有选择的摘要。确认后写入配置和凭据存储。完成后 Holon 会打印确认摘要。

## 引导完成之后

引导完成后，Holon 配置即可使用：

```bash
# 检查配置
holon config doctor

# 查看默认模型
holon config get model.default

# 启动 daemon 并创建第一个 Agent
holon daemon start
holon agent create my-first-agent
```

完整流程见[创建你的第一个 Agent](first-agent.md)。

## 凭据修复

如果已保存的凭据失效——例如 API key 过期或 OAuth token 被撤销——Holon 会在启动时检测到，
并建议重新运行引导：

```bash
holon onboard
```

向导会显示哪个提供商的凭据有问题，并引导你更新。其他提供商的已有配置不受影响。

当凭据修复针对的是已配置的提供商时，向导在覆盖前会要求确认——它会显示受影响的提供商并请你确认更新。

## 非交互式诊断

如果不想进入交互式向导，只想查看引导状态，使用 `--json`：

```bash
holon onboard --json
```

这会打印机器可读的引导报告，包含每部分的状态：

- `configured` — 已配置且工作正常
- `missing` — 尚未配置
- `unavailable` — 提供商不可达
- `restricted` — 部分配置完成，需要处理
- `skipped` — 有意跳过或不适用
- `failed` — 配置尝试失败，需要修复

每部分包含 `summary` 字符串、可选的 `details`，以及带建议 CLI 命令的 `actions`。

## 配置文件

| 文件 | 用途 |
|------|---------|
| `~/.holon/config.json` | 提供商定义、模型默认值、搜索设置 |
| `~/.holon/credentials.json` | 加密的凭据配置（API key、OAuth token） |

引导配置会写入这两个文件。不要直接编辑凭据文件；请使用 `holon onboard` 或
`holon config credentials`。

## CLI 参考

```
holon onboard [--json]
```

| 标志 | 说明 |
|------|-------------|
| `--json` | 以 JSON 打印引导诊断并退出（非交互） |

## 相关阅读

- [创建你的第一个 Agent](first-agent.md) — 从安装到第一条提示的完整流程
- [配置参考](/reference/configuration.md)（英文）— 配置文件结构与凭据管理
- [模型参考](/reference/models.md)（英文）— 支持的模型与提供商详情
- [故障排查](/guides/troubleshooting.md)（英文）— 诊断常见安装问题
