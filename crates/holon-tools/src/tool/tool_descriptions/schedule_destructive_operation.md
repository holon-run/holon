Schedule an explicitly authorized destructive lifecycle operation outside the
current Holon daemon cgroup. The operation is durably fenced before dispatch
and idempotent by `operation_id`; recovery verifies the target instead of
replaying the destructive command.
