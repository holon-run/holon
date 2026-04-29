# Work-Queue Prompt Projection

Decision:

- project work-item state into prompt context only when persisted work items
  already exist
- inject the active work item and plan in full
- inject queued and waiting items only as compact summaries

Reason:

- prompt projection should reflect persisted state, not invent it
- the early rollout remains message-driven first
