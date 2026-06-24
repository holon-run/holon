---
title: TUI guide
summary: Interactive terminal UI for Holon — navigation, slash commands, event log, model selection, and remote connection.
order: 12
---

# TUI Guide

Holon's terminal UI (`holon tui`) is the primary interactive interface for
day-to-day work. It runs inside your terminal and supports agent switching,
model selection, event inspection, and remote daemon connections.

## Starting the TUI

```bash
holon tui
```

Start with alternate screen disabled (useful when the terminal renders
incorrectly):

```bash
holon tui --no-alt-screen
```

Connect to a remote Holon daemon:

```bash
holon tui --connect https://your-server:8787 --token "your-token"
holon tui --connect https://your-server:8787 --token-file ~/.holon/token
holon tui --connect https://your-server:8787 --token-profile my-profile
```

| Option | Description |
|--------|-------------|
| `--no-alt-screen` | Disable alternate screen buffer |
| `--connect <URL>` | Connect to a remote daemon |
| `--token <TOKEN>` | Bearer token for remote connection |
| `--token-file <FILE>` | Read token from a file |
| `--token-profile <PROFILE>` | Use a stored token profile |

## Basic Navigation

The TUI is keyboard-driven. Type your message in the prompt area at the bottom
and press `Enter` to send. Use `Shift+Enter` to insert a newline without
sending.

Key bindings:

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Shift+Enter` | Insert newline |
| `↑` / `↓` | Navigate input history |
| `Esc` | Dismiss overlay or slash menu |
| `Ctrl+C` | Quit |
| `/` | Open slash command menu |

## Slash Commands

Type `/` in the prompt to open the slash command menu. Use `↑`/`↓` to navigate
and `Enter` to select. Press `Esc` to dismiss.

### Agent Commands

| Command | Description |
|---------|-------------|
| `/agents` | Open agent picker overlay |
| `/agent switch <id>` | Switch to a different agent |
| `/agent create <name>` | Create a new agent |
| `/agent start [id]` | Start an agent |
| `/agent stop [id]` | Stop an agent |
| `/model` | Open model picker for selected agent |
| `/state` | Open agent state overlay |
| `/abort` | Abort current agent run |

### Navigation Commands

| Command | Description |
|---------|-------------|
| `/help` | Show slash command help |
| `/events` | Open raw events overlay |
| `/transcript` | Open transcript overlay |

### Runtime Commands

| Command | Description |
|---------|-------------|
| `/tasks` | Open task overlay |
| `/refresh` | Refresh selected agent |
| `/clear-status` | Clear local status line |
| `/display <mode>` | Set chat display mode (`info`, `verbose`, `debug`, or numeric 3–5) |

### Skills Commands

Manage skills from the TUI:

| Command | Description |
|---------|-------------|
| `/skills` | Show enabled skills for the selected agent |
| `/skill-catalog` | Browse the Skill Library catalog |
| `/skill-add <source>` | Add a skill to the library |
| `/skill-remove <name>` | Remove a skill from the library |
| `/skill-enable <name>` | Enable a known skill for the agent |
| `/skill-disable <name>` | Disable a skill for the agent |

> `/skill-install` and `/skill-uninstall` are no longer the primary
> slash commands. Use `/skill-add` and `/skill-enable` to add and
> activate skills, or `/skill-remove` and `/skill-disable` to remove
> and deactivate them.

### Debug Commands

| Command | Description |
|---------|-------------|
| `/debug-prompt` | Open debug prompt dialog |

## Event Log

Use `/events` to open the raw event log overlay. This shows the underlying
runtime events (agent messages, task lifecycle, control-plane operations) as
they flow through the system. The overlay supports paging through event history.

## Model Selection

Use `/model` to open the model picker overlay. It lists available models and
lets you switch the selected agent's model without leaving the TUI. Model
changes take effect on the next agent run.

The model picker respects your configured providers. Manage them with:

```bash
holon config providers list
holon config models list
```

## Display Modes

Use `/display <mode>` to control how much internal detail appears in the chat
view:

| Mode | What you see |
|------|-------------|
| `info` | User-facing responses only |
| `verbose` | Includes tool calls and intermediate steps |
| `debug` | Full internal traces, events, and diagnostics |
| `3`–`5` | Numeric verbosity levels (3 = info, 4 = verbose, 5 = debug) |

## Remote Connection

When Holon runs as a daemon on a remote machine, connect with `--connect`:

```bash
holon daemon start --access tunnel   # on the remote machine
holon tui --connect https://your-server:8787 --token "your-token"
```

The daemon must be started with an access mode that accepts remote connections
(`tunnel` or `public`). Use `--access local` for local-only TUI connections.

## Troubleshooting

See the [Troubleshooting guide](/guides/troubleshooting#tui-issues) for common
TUI issues including garbled display and daemon connection problems.
