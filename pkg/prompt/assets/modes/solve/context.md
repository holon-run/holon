{{- if .ContextEntries }}
### SOLVE CONTEXT FILES
The following context is mounted at `/holon/input/context/`:
{{- range .ContextEntries }}
- {{ .Path }}{{ if .Description }} — {{ .Description }}{{ end }}
{{- end }}
{{- end }}

### SOLVE CONTEXT USAGE
- If `github/issue.json` exists, read it first to understand the task and any comments.
- If other provider context files are present, use them as authoritative requirements before modifying code.
