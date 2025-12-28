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
