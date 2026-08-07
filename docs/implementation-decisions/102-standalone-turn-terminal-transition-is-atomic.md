# Standalone Turn Terminal Transition Is Atomic

Decision:

- persist `AgentState.last_turn_terminal`, the terminal `TurnRecord`, and their
  audit events with one restricted runtime database transition
- use the same optimistic-concurrency retry boundary as queue terminal
  settlement
- keep cache updates and event publication as post-commit effects

Reason:

- standalone interactive, task-rejoin, and compatibility entrypoints must not
  leave a durable half-terminal turn when the process stops between writes
- restart replay must be idempotent and must not duplicate terminal audits
- fault injection after the AgentState write, TurnRecord write, audit writes,
  and before commit verifies that every durable write point rolls back together
