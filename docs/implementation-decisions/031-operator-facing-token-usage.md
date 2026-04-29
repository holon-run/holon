# Operator-Facing Token Usage

Decision:

- expose token usage as a first-class structured contract on operator-facing
  run and status surfaces
- preserve per-turn token usage on transcript and audit metadata

Reason:

- cumulative totals alone do not explain one recent model turn
- stable operator entry points should not require TUI work to inspect token use
