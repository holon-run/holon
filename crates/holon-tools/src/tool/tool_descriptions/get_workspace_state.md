Read the current agent's workspace lifecycle state. Returns attached workspace
bindings, the active projection and cwd, durable execution-root/worktree
records (including retained or removed artifacts), and active occupancy. The
model-facing receipt is a bounded summary that prioritizes the active root,
occupied roots, and recent roots; use `output_ref` when the complete canonical
evidence is required.
