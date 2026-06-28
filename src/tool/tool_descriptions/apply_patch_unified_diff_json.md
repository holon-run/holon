Apply a unified diff patch across one or more files. Call the JSON/function tool with exactly {"patch":"--- a/path\n+++ b/path\n@@ ..."}; do not use the Codex *** Begin Patch DSL as the advertised format.
Relative paths: write `"--- a/src/foo.rs"` (with `a/` prefix). Absolute paths: write `"--- /home/foo.rs"` directly (no `a/` prefix). Do not prefix absolute paths with `a/`.
