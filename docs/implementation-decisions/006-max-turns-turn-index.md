# max_turns counter uses turn_index instead of total_model_rounds

## Context

`--max-turns` limits the number of turns in a `run_once` invocation. The
counter previously compared `total_model_rounds` (cumulative provider calls
across all turns and rounds) against `max_turns`. This caused premature
termination when a single turn contained multiple model rounds (tool-use
loops), because a multi-round turn would consume multiple units of the budget
despite only using one turn.

## Decision

Changed the `max_turns` comparison from `total_model_rounds` to `turn_index`.
A turn is one logical agent execution unit; the number of model rounds within
that turn (driven by tool-use loops) is orthogonal to the turn budget.

Additionally, a cooperative budget warning is injected via the existing
runtime-reminder pipeline on the last allowed turn. This is a hint for the
agent to wrap up gracefully; enforcement remains purely in the scheduling
layer (the polling loop stops starting new turns after `turns_elapsed >=
max_turns`).

## Preserved boundary

The budget warning is not enforcement. The scheduling layer independently
guarantees that no new turn starts beyond `max_turns`.
