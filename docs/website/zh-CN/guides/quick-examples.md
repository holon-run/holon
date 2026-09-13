---
title: 快速示例
summary: 完成入门指南后可以尝试的常见 Holon 任务。
order: 15
---

# 快速示例

完成[入门指南](/zh-CN/getting-started/first-agent.md)后，可以试试这些常见的 Holon 任务。

## 1. 单次提问

提一个问题，不创建持久 Agent：

```bash
holon run "Explain how Rust ownership works in three sentences"
```

用 `--json` 获取机器可读输出：

```bash
holon run --json "List files in the current directory"
```

## 2. 创建 Agent

创建一个跨会话保留的命名 Agent：

```bash
holon agent create reviewer
```

从已安装或已同步的模板创建 Agent：

```bash
holon agent create reviewer --template code-reviewer
```

然后与它交互：

```bash
holon run --agent reviewer "Review the changes in src/runtime/turn.rs"
```

## 3. 切换模型

修改全局默认模型：

```bash
holon config set model.default "deepseek-anthropic@default/deepseek-v4-pro"
```

设置 Agent 级模型覆盖：

```bash
holon agent model set "anthropic@default/claude-sonnet-4-6" reviewer
```

查看某个 Agent 使用的模型：

```bash
holon agent model get reviewer
```

## 4. 以后台 daemon 运行

启动 daemon：

```bash
holon daemon start
```

查看 daemon 状态：

```bash
holon daemon status
```

查看 daemon 日志：

```bash
holon daemon logs
```

停止 daemon：

```bash
holon daemon stop
```

用指定访问模式启动：

```bash
holon daemon start --access tunnel
```

## 5. 使用终端 UI（TUI）

在本地启动交互式终端 UI：

```bash
holon tui
```

通过 TUI 连接远程 daemon：

```bash
holon tui --connect http://your-server:8787 --token "your-token"
```

或从文件读取 token：

```bash
holon tui --connect http://your-server:8787 --token-file ~/.holon/remote.token
```

使用已保存的 token profile：

```bash
holon tui --connect http://your-server:8787 --token-profile my-profile
```

如果终端渲染异常，禁用备用屏幕：

```bash
holon tui --no-alt-screen
```

在 TUI 中输入 `/` 打开斜杠命令菜单。用 `/model` 切换模型，用 `/agent` 管理 Agent，用 `/help` 查看所有命令。完整的斜杠命令参考见 [TUI 指南](/zh-CN/guides/tui)。

## 6. 启动 HTTP 服务器

把控制平面暴露为 HTTP API：

```bash
holon serve --port 8787
```

带访问控制：

```bash
holon serve --port 8787 --token "your-secret-token"
```

## 7. 配置 API 凭据

安全存储 API key（推荐方式）：

```bash
holon config credentials set --kind api_key --stdin deepseek
# Paste your API key and press Enter, then Ctrl+D
```

或使用环境变量：

```bash
export DEEPSEEK_API_KEY="sk-..."
holon run "Hello"
```

验证凭据是否可用：

```bash
holon config doctor
```

## 8. 检查配置

查看当前全部配置：

```bash
holon config list
```

查看所有可用配置项及其默认值：

```bash
holon config schema
```

列出已配置的提供商：

```bash
holon config providers list
```

列出可用模型：

```bash
holon config models list
```

## 9. 运行多轮任务

限制最大轮次数：

```bash
holon run --max-turns 5 "Write a Rust function that reverses a string, with tests"
```

在指定 workspace 中运行：

```bash
holon run --workspace-root /path/to/project "Analyze this codebase"
```

## 10. 用自定义 workspace 创建 Agent

```bash
holon agent create my-builder
holon run --agent my-builder --workspace-root /path/to/project "Build the project"
```

## 另见

- [入门指南](/zh-CN/getting-started/first-agent.md) — 完整的分步教程
- [配置参考](/zh-CN/reference/configuration.md) — 所有配置项与凭据管理
- [CLI 参考](/zh-CN/reference/cli.md) — 完整的命令行参考
- [故障排查](/zh-CN/guides/troubleshooting.md) — 常见问题与解决办法
- [集成指南](/zh-CN/guides/integration.md) — HTTP 控制平面集成
