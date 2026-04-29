# Context V1 And Compaction

Decision:

- keep durable append-only `messages.jsonl` and `briefs.jsonl`
- build model-visible context from structured working memory, episode memory,
  recent deltas, recent messages, recent briefs, and current input
- keep compaction deterministic and local rather than model-generated

Reason:

- deterministic memory derivation is easier to test and audit
- episode records preserve revision boundaries better than one rewritten
  summary blob
- durable history and model-visible context are different concerns
