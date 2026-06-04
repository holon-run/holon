---
title: RFC: TUI Vim Composer Mode
date: 2026-06-04
status: draft
Handle: rfc-tui-vim-composer-mode
---

# RFC: TUI Vim Composer Mode

## Summary

This RFC proposes an optional vim-style editing mode for the Holon TUI composer.

The central direction is:

- keep the default composer behavior unchanged
- add vim editing only to the prompt composer
- enable vim mode with a local `/vim` slash command
- keep the mode local to the current TUI session
- avoid changing runtime, daemon, storage, or configuration contracts

Vim mode should make the TUI more comfortable for operators who already use
modal terminal tools such as Codex, Claude Code, Vim, or Neovim, without making
Holon's TUI harder for default users.

## Related documents

- [TUI Command Surface](./tui-command-surface.md)
- [Implementation decision 058: TUI Vim Composer Mode](../implementation-decisions/058-tui-vim-composer-mode.md)

## Problem

The current TUI composer supports direct text input and readline-style editing
shortcuts. This is simple and works well for many users, but it is less natural
for operators whose terminal editing muscle memory is modal.

Those users expect basic behaviors such as:

- `Esc` to leave insert mode
- `h/j/k/l` movement
- `i`, `a`, `A`, `o`, and `O` insertion entry points
- word movement with `w`, `b`, and `e`
- line and word edits such as `dd`, `D`, `C`, and `cw`

Without a vim mode, these users must either avoid their normal editing habits or
fall back to external composition.

This is a TUI ergonomics problem, not a runtime model problem.

## Goals

- add optional vim-style composer editing
- preserve existing default composer behavior
- keep vim state local to the running TUI process
- expose the feature through the existing slash-command surface
- support a practical first subset of normal-mode editing
- keep slash commands and slash menu behavior compatible with vim mode
- avoid changing runtime trust, event, daemon, API, or persistent storage
  contracts

## Non-goals

- do not implement a full Vim emulator
- do not add visual mode, registers, counts, macros, search, paste/yank, or
  command-line `:` mode in the first version
- do not apply modal editing to overlays, model picker, task views, or event
  inspectors in the first version
- do not persist the setting in `config.json`
- do not add a runtime configuration key for vim mode
- do not change how operator chat is submitted to agents

## Core Judgment

Vim mode should be a local composer editing mode, not a second TUI command
language.

That means:

- `/vim` toggles editor behavior for the composer
- slash commands remain the TUI command surface
- normal chat submission remains the operator input path
- vim normal-mode keys never become hidden runtime commands

This keeps the feature aligned with the TUI command-surface RFC: local UI
controls are acceptable, but the composer should not become a shell, runtime
admin console, or privileged control plane.

## Command Surface

The TUI should add one slash command:

- `/vim`

The command has no arguments in the first version.

When vim mode is disabled:

- running `/vim` enables vim composer mode
- the composer enters normal mode
- the status bar shows a persistent `VIM NORMAL` / `VIM INSERT` hint
When vim mode is enabled:

- running `/vim` disables vim composer mode
- the composer returns to default editing behavior
- the status line reports that vim mode is disabled

The setting is intentionally session-local. Restarting the TUI restores the
default composer behavior.

## Composer Modes

Vim mode introduces two local composer submodes:

- insert mode
- normal mode

Insert mode accepts typed characters into the composer. Normal mode interprets
vim editing keys.

The default TUI editing path remains separate and unchanged when vim mode is
disabled.

## First-Version Key Behavior

### Mode switching

- `Esc`: enter normal mode from insert mode
- `i`: enter insert mode before the cursor
- `a`: move right when possible, then enter insert mode
- `A`: move to end of current line, then enter insert mode
- `o`: open a new line below, then enter insert mode
- `O`: open a new line above, then enter insert mode

### Movement

- `h`: move left
- `j`: move down
- `k`: move up
- `l`: move right
- `0`: move to start of current line
- `$`: move to end of current line
- `w`: move to next word start
- `b`: move to previous word start
- `e`: move to current or next word end

### Editing

- `x`: delete character under cursor
- `dd`: delete current line
- `D`: delete to end of current line
- `C`: delete to end of current line, then enter insert mode
- `cw`: delete through the current word, then enter insert mode
- `u`: restore the previous composer edit snapshot

### Submission

- `Enter` submits the composer from normal mode when the buffer is non-empty
- insert mode preserves the existing Enter behavior, including current
  multi-line paste handling and Shift+Enter newline behavior

## Slash Command Interaction

Slash commands remain single-shot TUI commands.

Vim mode must not break these existing rules:

- typing `/` starts a slash command draft
- the slash menu can still complete and execute slash commands
- `//text` can still send slash-prefixed chat text
- slash commands remain single-line commands
- slash command errors remain local status errors

When the composer is in vim normal mode, `/` should enter insert mode and insert
`/`, allowing the existing slash command flow to take over.

## Status And Help

The TUI should make the active editing mode visible enough that operators do
not have to infer it from key behavior.

Recommended status hints:

- default mode: keep the current status hint
- vim insert: show `VIM INSERT`
- vim normal: show `VIM NORMAL`

`/help` should list `/vim` and summarize the supported first-version keys
without attempting to document full Vim behavior.

## Implementation Boundary

The implementation should stay in the TUI layer.

Expected affected areas:

- composer editing state and helpers
- TUI app local state
- TUI input handling
- key resolution or vim-specific input dispatch
- slash command list and help text
- focused TUI/composer tests

The implementation should not affect:

- runtime scheduler
- message envelopes
- trust classification
- daemon HTTP API
- persistent storage
- runtime configuration schema
- agent state

## Testing Expectations

The PR should include focused tests for:

- `/vim` toggling state and status text
- normal and insert mode switching
- movement keys over single-line, multi-line, and UTF-8 text
- `x`, `dd`, `D`, `C`, `cw`, and `u`
- Enter submission from normal mode
- existing default composer behavior when vim mode is disabled
- slash menu behavior while vim mode is enabled

The normal Rust checks should still pass:

- `cargo fmt --all -- --check`
- `RUSTFLAGS="-D warnings" cargo check --all-targets`
- `cargo test --all-targets -- --test-threads=1`

## Related RFCs

- `tui-command-surface.md`
- `operator-display-levels-and-event-presentation.md`
- `runtime-configuration-surface.md`

## Decision

Holon should support an optional composer-only vim mode in the TUI.

The first version should be deliberately narrow:

- local session toggle through `/vim`
- practical normal/insert composer editing
- no persistent config
- no runtime contract changes
- no full Vim emulation

This improves terminal-operator ergonomics while preserving Holon's clear
separation between local UI behavior, operator input, and runtime semantics.
