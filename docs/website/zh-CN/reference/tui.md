---
title: TUI
summary: 终端 UI 参考：斜杠命令、快捷键、面板和连接控制。
order: 36
---

# TUI 指南

Holon 的终端 UI（`holon tui`）是日常工作的主要交互界面。它运行在你的终端里，支持 Agent 切换、模型选择、事件查看和远程 daemon 连接。

## 启动 TUI

```bash
holon tui
```

禁用备用屏幕启动（终端渲染异常时有用）：

```bash
holon tui --no-alt-screen
```

连接远程 Holon daemon：

```bash
holon tui --connect https://your-server:8787 --token "your-token"
holon tui --connect https://your-server:8787 --token-file ~/.holon/token
holon tui --connect https://your-server:8787 --token-profile my-profile
```

| 选项 | 说明 |
|--------|-------------|
| `--no-alt-screen` | 禁用备用屏幕缓冲区 |
| `--connect <URL>` | 连接远程 daemon |
| `--token <TOKEN>` | 远程连接的 Bearer token |
| `--token-file <FILE>` | 从文件读取 token |
| `--token-profile <PROFILE>` | 使用已保存的 token profile |

## 基本导航

TUI 用键盘操作。在底部的提示词区域输入消息，按 `Enter` 发送。用 `Shift+Enter` 插入换行而不发送。

按键绑定：

| 按键 | 操作 |
|-----|--------|
| `Enter` | 发送消息 |
| `Shift+Enter` | 插入换行 |
| `↑` / `↓` | 浏览输入历史 |
| `Esc` | 关闭浮层或斜杠菜单 |
| `Ctrl+C` | 退出 |
| `/` | 打开斜杠命令菜单 |

## 斜杠命令

在提示词区域输入 `/` 打开斜杠命令菜单。用 `↑`/`↓` 选择，`Enter` 确认。按 `Esc` 关闭。

### Agent 命令

| 命令 | 说明 |
|---------|-------------|
| `/agents` | 打开 Agent 选择浮层 |
| `/templates` | 打开 Agent 模板目录浮层 |
| `/agent switch <id>` | 切换到其他 Agent |
| `/agent create <name>` | 创建新 Agent |
| `/agent start [id]` | 启动 Agent |
| `/agent stop [id]` | 停止 Agent |
| `/agent delete [id]` | 删除 Agent（可加 `--cascade-private-children`） |
| `/model` | 为选中的 Agent 打开模型选择器 |
| `/state` | 打开 Agent 状态浮层 |
| `/abort` | 中止当前 Agent 运行 |

### 导航命令

| 命令 | 说明 |
|---------|-------------|
| `/help` | 显示斜杠命令帮助 |
| `/events` | 打开原始事件浮层 |
| `/transcript` | 打开对话记录浮层 |

### 运行时命令

| 命令 | 说明 |
|---------|-------------|
| `/tasks` | 打开任务浮层 |
| `/refresh` | 刷新选中的 Agent |
| `/clear-status` | 清除本地状态行 |
| `/onboard` | 通过 daemon 配置运行时默认模型 |
| `/vim` | 切换 vim 输入模式 |
| `/display <mode>` | 设置或重置聊天显示模式（`info`、`verbose`、`debug`、`3`–`5` 或 `reset`） |

### Skills 命令

在 TUI 中管理 skill：

| 命令 | 说明 |
|---------|-------------|
| `/skills` | 显示选中 Agent 已启用的 skill |
| `/skill-catalog` | 浏览 Skill Library 目录 |
| `/skill-add <source>` | 向库中添加 skill |
| `/skill-remove <name>` | 从库中移除 skill |
| `/skill-enable <name>` | 为 Agent 启用已知 skill |
| `/skill-disable <name>` | 为 Agent 禁用 skill |

> `/skill-install` 和 `/skill-uninstall` 不再是主要的斜杠命令。添加并
> 激活 skill 用 `/skill-add` 和 `/skill-enable`，移除并停用用
> `/skill-remove` 和 `/skill-disable`。

### 调试命令

| 命令 | 说明 |
|---------|-------------|
| `/debug-prompt` | 打开调试提示词对话框 |

## 事件日志

用 `/events` 打开原始事件日志浮层。它展示流经系统的底层运行时事件（Agent 消息、任务生命周期、控制平面操作）。浮层支持翻页浏览事件历史。

## 模型选择

用 `/model` 打开模型选择浮层。它列出可用模型，让你不离开 TUI 就能切换选中 Agent 的模型。模型变更在下次 Agent 运行时生效。

模型选择器会遵循你配置的提供商。用这些命令管理：

```bash
holon config providers list
holon config models list
```

## 显示模式

用 `/display <mode>` 控制聊天视图中显示多少内部细节：

| 模式 | 你会看到 |
|------|-------------|
| `info` | 仅面向用户的回复 |
| `verbose` | 包含工具调用和中间步骤 |
| `debug` | 完整的内部轨迹、事件和诊断 |
| `3`–`5` | 数字详细级别（3 = info，4 = verbose，5 = debug） |

## 远程连接

当 Holon 作为 daemon 运行在远程机器上时，用 `--connect` 连接：

```bash
holon daemon start --access tunnel   # 在远程机器上
holon tui --connect https://your-server:8787 --token "your-token"
```

daemon 必须以接受远程连接的访问模式启动（`tunnel`、`lan` 或 `tailnet`）。本地 TUI 连接用 `--access local`。

## Agent 模板

TUI 支持直接在终端里浏览、安装模板并从模板创建 Agent，常见的模板工作流不必再切到 Web GUI 或 CLI。

- **浏览模板** —— 运行 `/templates` 打开模板目录浮层。用 `↑`/`↓` 浏览已安装的模板，按 `Enter` 选中一个模板来创建 Agent。
- **从 URL 安装** —— 在模板浮层里按 `g` 输入 GitHub 模板 URL，将其安装到用户全局模板库。
- **移除** —— 按 `r` 移除选中的模板。
- **同步** —— 按 `s` 从配置的远程来源同步模板。
- **不用模板创建** —— 按 `n` 跳过模板选择，用默认配置创建 Agent。

## 浮层快捷键前缀

所有浮层快捷键现在都用统一的 `Ctrl+O` 前缀，取代了以前的单键绑定。这能避免正常打字时误开浮层。

按 `Ctrl+O` 后，状态行会显示简短提示。按对应按键打开相应浮层：

| 按键 | 浮层 |
|-----|---------|
| `H` | 帮助 |
| `A` | Agent 选择器 |
| `T` | 任务 |
| `S` | Agent 状态 |
| `C` | 对话记录 |
| `E` | 事件日志 |
| `M` | 模型选择器 |
| `K` | 选中 Agent 的 skill |

按 `Esc` 取消前缀，继续输入。

## 持久显示状态

TUI 会记住你上次的显示模式，并在重启后恢复。显示模式按 Agent 存储在 `~/.holon/state/tui/` 下的 `local.json` 中（远程连接则是 `remote-<hash>.json`），所以每个 Agent 保留自己的偏好。这适用于通过 `/display <mode>` 设置的聊天显示模式（info、verbose、debug）。

## 故障排查

常见 TUI 问题（包括显示乱码和 daemon 连接问题）见[故障排查指南](/zh-CN/guides/troubleshooting#tui-issues)。
