# Commit ordinary tool results together

The tool loop previously committed evidence, execution Audit and the completed
external-command barrier through separate mutation entries. The barrier itself
also emitted a separate checkpoint Audit. Repeated owner preparation and writer
admission added work between a finished tool and the next model round.

Use one typed Evidence command containing the original Tool, Audit and optional
Turn barrier. Freeze the prior Turn digest, then validate and persist all records
with one receipt and publication. Keep the same Audit order and original
Input/Current qualification. A failed Audit or receipt must also roll back the
Tool and barrier. Recovery installs the original ACK, and exact replay preserves
later Turn changes. Wait and completion tools continue to join their existing
terminal commits.

This combines the durability boundary; it does not acknowledge a result early or
send its required records to a background best-effort writer.
