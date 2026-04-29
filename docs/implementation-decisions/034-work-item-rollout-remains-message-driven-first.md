# Work-Item Rollout Remains Message-Driven First

Decision:

- keep the existing message-driven runtime path intact during early work-item
  rollout
- add `WorkItem` as an explicit higher-level container without forcing every
  ingress through a semantic resolver

Reason:

- this keeps migration incremental
- it avoids introducing a second resolver agent just to classify arbitrary
  ingress text
