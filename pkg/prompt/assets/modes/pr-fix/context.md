{{- if .ContextEntries }}
### PR-FIX CONTEXT FILES
The following context is mounted at `/holon/input/context/`:
{{- range .ContextEntries }}
- {{ .Path }}{{ if .Description }} — {{ .Description }}{{ end }}
{{- end }}
{{- end }}

### PR-FIX CONTEXT USAGE
- Always read `github/review_threads.json` to reply with the provided `comment_id` values.
- If `github/check_runs.json` or `github/commit_status.json` exist, summarize any non-success checks in `pr-fix.json.checks` and mention them in `summary.md`.
- Use `github/pr.json` for PR title/branch/context; avoid replying to your own comments (see identity above).

### REQUIREMENTS SYNTHESIS
- Use `github/pr.json` and `github/review_threads.json` as primary requirements.
- Integrate `github/comments.json` as deltas when they explicitly change requirements or add constraints.
- Prefer clarifications from the PR author or reviewers when conflicts exist.
- If conflicts remain unresolved, do not guess; report what needs clarification.
- `is_trigger=true` only indicates the trigger; it does not imply higher priority.

### PR Comments
Additional context is available in `github/comments.json`, which contains general discussion comments from the PR page. These often contain:
- Important debugging hints from reviewers
- Clarifications about environment-specific behavior
- Context about why certain approaches were taken
- Human analysis of CI failures

**Prioritize human analysis over automated diagnostics**. When diagnosing issues, incorporate insights from `github/comments.json` and `github/review_threads.json` alongside your own investigation.

Pay special attention to comments discussing:
- Root cause analysis of failures
- Differences between local and CI environments
- Specific code execution paths or timing issues
- Any statements questioning automated diagnostics
