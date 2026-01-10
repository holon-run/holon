{{- if .ContextEntries }}
### SOLVE CONTEXT FILES
The following context is mounted at `/holon/input/context/`:
{{- range .ContextEntries }}
- {{ .Path }}{{ if .Description }} — {{ .Description }}{{ end }}
{{- end }}
{{- end }}

### SOLVE CONTEXT USAGE
- Read `github/issue.json` first to understand the task and baseline context.
- Read `github/comments.json` for discussion and updates.
- If other provider context files are present, use them as authoritative requirements before modifying code.

### REQUIREMENTS SYNTHESIS
- Build a single requirement list by reconciling the issue body with all human comments.
- Treat comments as deltas: only override earlier requirements when a comment explicitly changes them.
- Prefer clarifications from the issue author or assignee when conflicts exist.
- If comments conflict without a clear resolution, do not guess; report what needs clarification.

### COMMENT FILTERING
- Ignore bot/deploy/status comments unless they contain explicit tasks.
- `is_trigger=true` only indicates the trigger; it does not imply higher priority.
