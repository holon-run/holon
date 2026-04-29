# Turn-End Work-Item Commit Uses A Bound Active Snapshot

Decision:

- bind each interactive turn to the active work item snapshot visible at turn
  start
- commit turn-end transitions only against that bound item
- keep runtime fact checks intentionally small and conservative

Reason:

- turn-end commit should resolve against the same runtime object the model saw
- unrelated queue changes should not leak into the wrong turn settlement
