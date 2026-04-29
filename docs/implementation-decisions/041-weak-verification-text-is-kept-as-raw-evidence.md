# Weak Verification Text Is Kept As Raw Evidence

Decision:

- remove `latest_verified_result` from working memory
- remove `result_hint` from turn deltas and archived episode memory
- keep verification evidence available through raw recent briefs and tool
  executions

Reason:

- recent "verified" text is still a heuristic, not a structured runtime fact
- weak verification evidence is useful without being promoted into truth
