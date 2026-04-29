# Turn Terminal Settlement Before Closure

Decision:

- persist a turn-terminal record for each completed or aborted interactive turn
- make `run_once` and child-task rejoin depend on that turn-terminal state
- remove the extra terminal-delivery model round

Reason:

- terminal settlement is a runtime fact and should not depend on text
- one-shot runs and child-agent rejoin need the same lower-level notion of
  turn completion
