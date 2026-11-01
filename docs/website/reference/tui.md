---
title: TUI
summary: Terminal UI reference — slash commands, keyboard shortcuts, panels, and connection controls.
order: 36
---

# TUI

Holon's terminal UI (`holon tui`) is the primary interactive interface for
day-to-day work. It runs inside your terminal and supports agent switching,
model selection, event inspection, and remote daemon connections.

## Starting the TUI

```bash
holon tui
```

Select an exact public agent ID for this session, locally or remotely:

```bash
holon tui --agent worker
holon tui --connect https://your-server:8787 --token-profile my-profile --agent worker
```

`--agent` overrides remembered/default selection. It checks the public roster
and compact status before raw mode or alternate screen; unavailable or
inaccessible targets fail rather than silently selecting another agent. Viewing
an unloaded or stopped public agent does not start it. The startup constraint
ends after the first successful state bootstrap, allowing normal switching and
runtime fallback. The entire session, including later manual switches, does not
update remembered selection. Without the flag, selection memory is unchanged.

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
| `--agent <AGENT_ID>` | Select an exact public agent ID; keep all selection changes session-only |
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
| `/templates` | Open agent template catalog overlay |
| `/agent switch <id>` | Switch to a different agent |
| `/agent create <name>` | Create a new agent |
| `/agent start [id]` | Start an agent |
| `/agent stop [id]` | Stop an agent |
| `/agent delete [id]` | Delete an agent (optionally `--cascade-private-children`) |
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
| `/onboard` | Configure the runtime default model through daemon config |
| `/vim` | Toggle vim composer editing |

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

## Conversation

The agent picker lists public agents, like the Web GUI. Private subagents are
not fetched as part of the default roster.

One conversation page combines operator inputs, Briefs, and the current turn's
execution activity. A running turn has one Working header, with bounded tool
parameters/results below it. Execution stop removes the running indicator but
keeps static details until the Brief body is readable; then details fold while
the Brief remains outside the fold. A Brief received before execution finishes
does not hide active work.
Turns without a Brief show a safe result/status notice instead of an invented
response.

Briefs and terminal status notices are static, not "Working" indicators.
If execution finishes before result delivery, the page keeps an awaiting-result
notice. Failed Brief reads retry with capped backoff even when the stream is
otherwise idle.

Press `Ctrl+O`, then `D` to inspect turns, tool records and task results.
Use `↑`/`↓` to select a turn, `Enter` to fold/unfold or retry, and
`PgUp`/`PgDn` to scroll details. Manual expansion is preserved.
Historical details load on demand; task cards distinguish completed work from
results queued for consumption and retain access to the original child reply.
Scrolling into history keeps a stable reading position during updates; `Ctrl+End`
resumes tail-follow. Narrow status lines use visible ellipses, with full
workspace/model details available in `Ctrl+O S`.

History loads turn summaries rather than verbose event history. Use `/events`
for separate raw-event inspection. There is no `/display` command or verbosity
level to select for the conversation.

## Remote Connection

When Holon runs as a daemon on a remote machine, connect with `--connect`:

```bash
holon daemon start --access tunnel   # on the remote machine
holon tui --connect https://your-server:8787 --token "your-token"
```

The daemon must be started with an access mode that accepts remote connections
(`tunnel`, `lan`, or `tailnet`). Use `--access local` for local-only TUI
connections.

## Agent Templates

The TUI supports browsing, installing, and creating agents from templates
directly within the terminal. This avoids switching to the Web GUI or CLI
for common template workflows.

- **Browse templates** — Run `/templates` to open the template catalog
  overlay. Navigate installed templates with `↑`/`↓` and press `Enter` to
  select a template for agent creation.
- **Install from URL** — Press `g` inside the templates overlay to enter a
  GitHub template URL and install it into the user-global template library.
- **Remove** — Press `r` to remove a selected template.
- **Sync** — Press `s` to sync templates from configured remote sources.
- **Create without template** — Press `n` to skip template selection and
  create an agent with the default configuration.

## Overlay Shortcut Prefix

All overlay shortcuts now use a common `Ctrl+O` prefix, replacing the
previous single-key bindings. This prevents accidental overlay openings
during normal typing.

After pressing `Ctrl+O`, a short hint appears in the status line. Press a
target key to open the corresponding overlay:

| Key | Overlay |
|-----|---------|
| `H` | Help |
| `A` | Agent picker |
| `T` | Tasks |
| `D` | Turn/tool details and task results |
| `S` | Agent state |
| `C` | Transcript |
| `E` | Event log |
| `M` | Model picker |
| `K` | Selected-agent skills |

Press `Esc` to cancel the prefix and continue typing.

## Previous Display Preferences

Older versions stored per-agent display modes under `~/.holon/state/tui/`.
Those preferences are no longer used: conversation detail visibility follows
the canonical turn lifecycle, not a saved display level.

## Troubleshooting

See the [Troubleshooting guide](/guides/troubleshooting#tui-issues) for common
TUI issues including garbled display and daemon connection problems.
