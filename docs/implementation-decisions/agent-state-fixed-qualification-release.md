# Fixed qualification is part of one release

The Episode qualification schema (v82), Control obligations (v83), input/evidence qualification (v84), and original input-mutation receipts (v85) ship together in this change. Intermediate development commits are not independently released database formats.

The fixed upgrade lease captures existing legacy scopes at v82 and qualifies their bounded input/evidence metadata at v84. A newly created or recreated identity receives `fresh_empty` continuity and cannot adopt records left under the same agent name. Reopening the database never widens either fixed checkpoint to include later records.

The checkpoint freezes canonical body identity separately from observer revisions. Turn records have no canonical content hash, so the checkpoint includes their bounded body digest before any later first materialization. Intermediate development databases missing that digest are not adopted on read; releasing such a format separately would require an explicit migration policy as well.

If v82 is released separately in the future, upgrading that release requires an explicit input/evidence adoption actor and policy. Treating all v82 `fresh_empty` scopes as legacy would violate identity isolation. Historical reads remain separate from current-scope qualification.
