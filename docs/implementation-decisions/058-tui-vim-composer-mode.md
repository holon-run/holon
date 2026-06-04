# TUI Vim Composer Mode

## Choice

Vim mode is implemented as local TUI composer state rather than runtime
configuration or agent state.

## Reason

The feature changes how one TUI session edits the prompt buffer. It does not
change operator message provenance, daemon APIs, runtime scheduling, persistent
storage, or agent behavior. A session-local `/vim` toggle keeps the change
small and consistent with the TUI command surface.

## Preserved Boundary

Slash commands remain the TUI command surface, and normal chat submission
remains the operator input path. Vim normal-mode keys edit the local composer
only; they do not become hidden runtime commands.
