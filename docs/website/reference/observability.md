---
title: Observability
summary: Trace export, protected metrics endpoints, OTLP fields, and runtime performance snapshot contracts.
order: 40
---

# Observability

For deployment and a verified test path, use
[Configure observability](../guides/configure-observability.md). For one task's
progress and result, use [Observe agent work](../guides/observe-agent-work.md).

Holon keeps telemetry work bounded:

- completed spans enter an in-memory recent ring;
- selected slow, failed, cancelled, or sampled traces may be retained in
  `diagnostics.sqlite`;
- optional OTLP export uses a bounded non-blocking queue;
- OpenMetrics exposes a fixed set of label-free `holon_*` series.

Exporter or collector failure does not block turns, tools, persistence, or
delivery. Exporter drop and failure counters identify degraded telemetry.

## Export traces with OTLP/HTTP

The configuration object is `runtime.observability.otlp` in
`<HOLON_HOME>/config.json`:

```json
{
  "enabled": true,
  "endpoint": "http://127.0.0.1:4318/v1/traces",
  "queue_capacity": 1024,
  "batch_size": 128,
  "batch_interval_ms": 1000,
  "timeout_ms": 5000
}
```

| Field | Contract |
|---|---|
| `enabled` | Enables the optional exporter; disabled by default |
| `endpoint` | Explicit HTTP or HTTPS trace endpoint, required when enabled |
| `queue_capacity` | Bound on the non-blocking export queue |
| `batch_size` | Export batch size |
| `batch_interval_ms` | Batch interval in milliseconds |
| `timeout_ms` | Export request timeout in milliseconds |
| `credential_profile` | Optional credential-store profile used for bearer authentication |
| `headers` | Static non-secret headers; do not combine an `authorization` header with `credential_profile` |

Restart the daemon after changing OTLP settings. Store collector tokens with
`holon config credentials set --kind bearer_token --stdin PROFILE`; reference the
profile rather than writing the token into configuration.

The baseline Collector configuration at
`docs/website/assets/observability/otel-collector.yaml` accepts OTLP/HTTP JSON on
port `4318` and uses the debug exporter. It listens on `0.0.0.0`; restrict its
bind address or network access for your deployment.

## Scrape protected OpenMetrics

| Property | Value |
|---|---|
| Route | `GET /api/control/runtime/metrics` |
| Authentication | Same control-plane authentication as other `/api/control/*` routes |
| Content type | `application/openmetrics-text; version=1.0.0; charset=utf-8` |
| Terminator | `# EOF` |
| Cardinality | Fixed series-count ceiling, no labels |

Trace IDs, agent IDs, model names, URLs, error text, and other high-cardinality
values never become metric labels. Keep the control endpoint on loopback or a
private network. Do not embed its bearer token in dashboard JSON or scrape
configuration text.

The baseline `docs/website/assets/observability/prometheus.yaml` reads the token
from `/run/secrets/holon-control-token`, scrapes `127.0.0.1:7878`, and loads rules
from `/etc/prometheus/holon-alerts.yaml`. These are deployment paths to configure,
not files Holon installs automatically.

## Install the dashboard and alerts

The [setup guide](../guides/configure-observability.md#4-add-prometheus-and-grafana)
contains installation and validation steps. The maintained assets are:

| Asset | Contents |
|---|---|
| `docs/website/assets/observability/grafana-dashboard.json` | Uptime, turn p50/p95/p99, projection-gate pressure, diagnostics writer queue/drops/failures, and OTLP queue/export/drop/failure panels |
| `docs/website/assets/observability/holon-alerts.yaml` | Baseline Prometheus alert rules |

Alert thresholds are starting points, not universal service-level objectives.
Tune turn latency and alert duration for the observed workload.

## Diagnose telemetry failures

| Signal | What it distinguishes |
|---|---|
| `holon_process_uptime_seconds` | Reachable scrape target and process age |
| `holon_otlp_exporter_failed_batches_total` | Collector, credentials, TLS, or endpoint failures |
| `holon_otlp_exporter_dropped_spans_total` | Lost exported telemetry under pressure |
| `holon_otlp_exporter_queue_depth` | Pending export backlog |
| `holon_diagnostics_writer_dropped_traces_total` | Dropped local retention work |
| `holon_diagnostics_writer_failures_total` | Local diagnostic persistence failures |

Local trace retention is independent of OTLP export. Use
`holon debug trace --search MESSAGE_OR_TURN_ID`, then `holon debug trace TRACE_ID`
for a matching retained trace. Sampling and retention policy determine whether
one exists. See the [diagnosis workflow](../guides/configure-observability.md#5-diagnose-a-missing-signal)
before changing or restarting telemetry services.

## Runtime performance snapshots

`GET /api/control/runtime/performance` returns a protected JSON snapshot of
process-lifetime performance counters. Restarting the daemon resets these
counters; snapshot comparisons should use the same process lifetime.

### Metric groups

| Group | Examples |
|---|---|
| `turn.*` | `turn.total`, `turn.context_build`, `turn.provider_round`, `turn.tool_execution`, `turn.cleanup` |
| `provider.*` | `provider.request_build`, `provider.round_total`, `provider.retry` |
| `tool.execution` | Cumulative tool timing |
| `storage.*` | `storage.append_event`, `storage.persist_state` |
| `projection.agent_state.*` | Task, timer, WorkItem, wait, and external-trigger projection work |
| `projection.agents_list` | Agent-list projection time |
| `http.*` | Per-route HTTP timing |
| `scheduler.*` | `scheduler.poll.all`, `.message`, `.idle`, `.stopped`, and other scheduler phases |

The snapshot includes additional subsystem counters. The table is a guide to
families, not an exhaustive registry.

### Metric fields

| Field | Meaning |
|---|---|
| `name` | Metric identifier |
| `count` | Recorded calls |
| `total_ms` | Cumulative elapsed milliseconds |
| `max_ms` | Maximum recorded duration |
| `avg_ms` | Mean duration |
| `p50_ms`, `p95_ms`, `p99_ms` | Histogram-derived duration percentiles |
| `total_bytes`, `avg_bytes` | Optional byte totals and mean for metrics recording byte counts |

Capture before/after snapshots for a reproducible operation rather than
restarting solely to clear metrics. A high idle-poll count can be normal when
no agents have runnable work.

## Related reference & concepts

- [HTTP control plane reference](http-control-plane.md)
- [Configuration reference](configuration.md)
- [Configure observability](../guides/configure-observability.md)
