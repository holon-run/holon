# Idle Activation Comes From The Persisted Work Queue

Decision:

- when the runtime is idle, consult the persisted work queue before sleeping
- continue an existing `active` item or promote one `queued` item to `active`
- keep the no-work-item case on the existing idle path

Reason:

- persisted work-item state should eventually drive liveness
- consulting the queue only when idle keeps the scheduler non-preemptive
