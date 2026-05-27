# Sleep As A Terminal Tool Round

Decision:

- treat a tool round containing only `Sleep` calls as a valid terminal state
- finalize the turn immediately instead of forcing another model round
- keep this as legacy compatibility; new model-facing tool catalogs do not
  advertise `Sleep`

Reason:

- providers may emit `Sleep` after already completing the user-facing answer
- without this rule the runtime can loop despite the task being effectively
  done
