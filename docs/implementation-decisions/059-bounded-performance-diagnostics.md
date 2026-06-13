# 059 Bounded Performance Diagnostics

Holon exposes first-phase runtime performance diagnostics as bounded in-process
counters, not as a full metrics subsystem.

The counters cover the current investigation hotspots: HTTP JSON response
construction, `agent_summary` projections, runtime DB connection opens, and
scheduler poll outcomes. The snapshot is available through the local control
plane and `holon debug performance`, so operators can inspect a running daemon
without scraping logs or attaching a profiler.

This deliberately preserves a small runtime boundary. The snapshot shape is
stable enough for local debugging and benchmark prompts, but it does not yet
commit Holon to Prometheus/OpenMetrics export, distributed tracing semantics, or
high-cardinality per-query metrics.
