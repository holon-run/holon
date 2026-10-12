# Bound command preparation before rendering

Resident tool evidence stores a small context projection and a canonical locator. It does not retain full tool input/output or command stdout merely to render a command preview.

For command text up to 64 KiB, projection preparation passes the complete command through the existing `command_preview` helper. This preserves its secret redaction and inline script/heredoc handling before the display limit is applied. Truncating raw command text before this helper could remove the syntax needed for safe preview generation.

Larger command text produces `[omitted: command exceeds context preview preparation limit]`. This is an explicit preparation boundary, not a statement that the command or its evidence is unavailable. The canonical input, output, authority, and full-reference locator retain their existing meaning. Explicit full evidence access follows its own authorization and byte limits.

The SQLite preparation query returns at most the bounded command field and scalar output metadata. Large stdout is not loaded into the resident projection. SQLite's internal JSON processing remains measured preparation work and is not counted as zero I/O.
