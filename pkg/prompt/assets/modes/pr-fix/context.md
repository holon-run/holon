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
- Use `github/pr.json` and `github/review.md` for PR title/branch/context; avoid replying to your own comments (see identity above).

### PR Comments
Additional context is available in `github/comments.json`, which contains general discussion comments from the PR page. These often contain:
- Important debugging hints from reviewers
- Clarifications about environment-specific behavior
- Context about why certain approaches were taken
- Human analysis of CI failures

**Prioritize human analysis over automated diagnostics**. When diagnosing issues:
1. First check `github/comments.json` for human insights
2. Then examine `github/review_threads.json` for code-specific feedback
3. Finally, perform your own investigation

Pay special attention to comments discussing:
- Root cause analysis of failures
- Differences between local and CI environments
- Specific code execution paths or timing issues
- Any statements questioning automated diagnostics
