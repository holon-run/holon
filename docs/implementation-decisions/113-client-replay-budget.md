# Event replay uses a client-owned composite budget

Retained history defines whether incremental recovery is possible, not whether
it is economical. The browser therefore bounds each replay attempt by an
estimated sequence gap plus actual pages, applied page events, serialized
response bytes, and elapsed time.

The defaults are a 10,000-sequence preflight gap, 50 pages, 10,000 events,
16 MiB, and 30 seconds. Exceeding any limit performs at most one authoritative
projection bootstrap. If replay after that snapshot also exceeds a limit, the
Agent remains out of live state with an explicit error. A budget-driven
bootstrap preserves the prior read marker but marks unread certainty truncated
at the skipped snapshot boundary.

That truncated generation retires itself once the read marker catches up with
the observed head: the client acknowledges automatically at the gated head the
marker reached, and the explicit acknowledgement stays available as an early
confirmation.
