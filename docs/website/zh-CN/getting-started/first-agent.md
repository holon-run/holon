---
title: 创建你的第一个 Agent
summary: 安装 Holon、配置模型，并从具名 Agent 获得经核实的交付结果。
order: 20
aliases:
  - /zh-CN/guides/quick-examples
  - /zh-CN/guides/quick-examples.md
---

<a id="run-your-first-holon-task"></a>

<a id="create-your-first-agent"></a>

# 创建你的第一个 Agent

本教程带你完整跑通从零开始的配置流程，并最终由一个具名 Agent 回答一个简单请求。在发送请求前先配置模型，随后无论在终端还是浏览器中都可以与同一个 Agent 交互。

<a id="prerequisites"></a>

## 前置条件

你需要一个终端环境以及对受支持模型提供商的访问权限。如果使用本地模型提供商，必须确保其模型服务已正常运行且网络可达。第一次执行无需准备模板、项目工作区或外部事件集成。

<a id="step-1-install-holon"></a>

## 步骤 1：安装 Holon

<a id="option-a-homebrew-recommended"></a>

### 方式 A：Homebrew 安装（推荐）

```bash
brew tap holon-run/tap
brew install holon
```

<a id="option-b-direct-binary-download"></a>

### 方式 B：直接下载二进制

从 [GitHub Releases](https://github.com/holon-run/holon/releases/latest) 下载适用于你操作系统与架构的二进制文件，根据需要添加执行权限，并放入系统的 `PATH` 路径中。

<a id="option-c-build-from-source"></a>

### 方式 C：源码编译

如果你打算对 Holon 自身进行开发，请遵循[本地编译工作流程](../maintainers/local-runtime.md)。

<a id="verify-the-install"></a>

### 验证安装

```bash
holon --help
```

若提示找不到命令，请在继续前检查并修正你的 `PATH` 环境变量。

<a id="step-5-configure-models"></a>
<a id="recommended-quick-setup-with-holon-onboard"></a>
<a id="7-set-up-api-credentials"></a>

<a id="step-2-configure-a-model"></a>

## 步骤 2：配置模型

```bash
holon onboard
holon config doctor
```

交互式向导会引导你选择模型提供商、配置凭据并选定模型。如果你在提示过程中需要指导，请参考[引导配置指南](onboarding.md)。在继续之前解决所有凭据缺失或模型不可用的报错；切勿在 Agent 对话中直接粘贴敏感密钥。

<a id="step-2-start-the-holon-runtime"></a>
<a id="option-b-daemon-mode-recommended-for-agents"></a>
<a id="4-run-as-a-background-daemon"></a>

<a id="step-3-start-the-runtime"></a>

## 步骤 3：启动运行时

```bash
holon daemon start
holon daemon status
```

<a id="verify-the-daemon-is-running"></a>

### 验证守护进程是否正常运行

检查状态命令是否报告守护进程处于运行中。守护进程负责托管 Agent 及其工作状态，支持各类客户端随时连接与断开。

<a id="step-4-create-an-agent"></a>
<a id="2-create-an-agent"></a>

<a id="step-4-create-a-named-agent"></a>

## 步骤 4：创建具名 Agent

选择一个未被使用的 ID；本示例使用 `first-helper`：

```bash
holon agent create first-helper
holon agent list
holon run --agent first-helper "用三句话解释 Rust 的所有权机制。不要修改任何文件。"
```

确认列表中包含 `first-helper`，且请求输出了有效解答。Agent 出现在列表中仅代表创建成功，不代表模型一定能正常应答；这句简短的请求同时对这两者进行了验证。如果报错，请检查错误提示并再次运行 `holon config doctor`。

<a id="the-default-agent"></a>

### 默认 Agent

全新的运行时也内置提供了 `main` Agent。你可以直接使用它，而无需专门创建具名 Agent。不过，具名 Agent 有助于将持续的独立职责与其他日常交互分离开来；详见 [Agent、角色与指引](../concepts/agents.md)。

<a id="create-a-named-agent"></a>
<a id="list-agents"></a>
<a id="step-3-connect-with-the-tui"></a>
<a id="5-use-the-terminal-ui-tui"></a>

<a id="step-5-continue-in-a-client"></a>

## 步骤 5：在客户端中继续交互

<a id="start-the-tui"></a>

### 启动终端 UI（TUI）

```bash
holon tui
```

<a id="tui-basics"></a>

### TUI 基础操作

在发送后续请求前，先选择 `first-helper`。输入 `/` 可打开命令菜单，输入 `/help` 可查看当前快捷键。有关界面导航和命令用法，请参阅 [TUI 参考](../reference/tui.md)。

<a id="basic-navigation"></a>
<a id="switch-agents-in-the-tui"></a>

你也可以使用 [Web GUI](../guides/use-web-gui.md)。无论使用哪种客户端，先确认当前选中的 Agent ID，以确保后续交互递送给同一个 Agent。

<a id="option-a-cli-mode-quick-start"></a>
<a id="1-one-shot-question"></a>

<a id="run-a-one-shot-question-instead"></a>

## 改为单次执行问答

对于不需要持久守护进程会话的有界请求，可以直接单次运行：

```bash
holon run "用三句话解释 Rust 的所有权机制"
```

单次运行可能包含多轮模型与工具交互才能最终退出。请不要将其用作需要等待 CI 或后续人工决策的跟踪手段；长生命周期工作请参阅[运行长生命周期任务](../guides/run-long-lived-task.md)。

<a id="stop-the-daemon"></a>

### 停止守护进程

所有工作结束后，可以通过 `holon daemon stop` 停止本地守护进程。不要单纯为了关闭某个客户端窗口而停止守护进程，因为其他 Agent 可能仍在后台推进任务。

<a id="next-steps"></a>

## 下一步

原“快速示例”页面现已重定向至此。若要了解初次运行之外的进阶工作流，请参阅下列专项指南：

<a id="use-a-template"></a>
- [使用模板创建 Agent](../guides/create-agent.md) 介绍在创建新 Agent 之前如何查找与准备角色模板。

<a id="3-switch-models"></a>
<a id="manual-setup-alternative"></a>
<a id="configuration-layers"></a>
<a id="set-provider-credentials"></a>
<a id="option-a-environment-variables-quick-start"></a>
<a id="option-b-credential-store-recommended-for-persistent-config"></a>
<a id="set-the-default-model"></a>
<a id="agent-level-model-override"></a>
<a id="verify-model-configuration"></a>
<a id="8-inspect-configuration"></a>
- [配置模型](../guides/configure-models.md) 涵盖手动配置凭据、按 Agent 指定模型、回退链机制以及配置诊断。

<a id="remote-tui-optional"></a>
- [连接远程运行时](../guides/connect-remote-runtime.md) 介绍访问模式、身份认证以及远程客户端连接。

<a id="6-start-the-http-server"></a>
- [通过 HTTP 自动化 Holon](../guides/automate-over-http.md) 涵盖通过 API 提交工作与追踪进度。

<a id="9-run-a-multi-turn-task"></a>
<a id="10-create-an-agent-with-a-custom-workspace"></a>
- [在项目工作区或 worktree 中工作](../guides/work-with-workspaces.md) 介绍项目绑定与改动隔离。[运行长生命周期任务](../guides/run-long-lived-task.md) 介绍需要持久化进度与等待的目标管理。

<a id="see-also"></a>

<a id="troubleshooting"></a>

## 故障排查

<a id="daemon-wont-start"></a>

### 守护进程无法启动

执行 `holon daemon status` 与 `holon daemon logs` 检查日志。根据报告的启动报错进行针对性排查，不要随意删除运行时数据或反复重复启动。

<a id="tui-cant-connect"></a>

### TUI 无法连接

确认守护进程正在目标主机上正常运行。对于远程连接，请遵循前述远程指南进行配置，不要假设本地套接字可以跨机器通信。

<a id="model-errors"></a>

### 模型报错

运行 `holon config doctor` 检查模型与凭据状态。可结合前述模型配置指南进行核对。如果请求已被受理但执行报错或停滞不前，请参阅[排查 Holon 任务问题](../guides/troubleshooting.md)。
