# Control-Plane Work-Item Enqueue

Decision:

- add a control route to enqueue queued work items directly
- persist the queued item without creating a normal transcript message
- do not let this route interrupt the current active work item

Reason:

- operators sometimes need to queue future work intentionally while the agent
  is already busy
- forcing that through prompt ingress would reintroduce semantic inference
