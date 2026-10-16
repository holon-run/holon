---
title: Configure observability
summary: Export traces, scrape protected metrics, and verify the telemetry path separately from task execution.
order: 270
---

# Configure observability

Use this guide to monitor the runtime across tasks. If you only need one
objective's progress or result, start with [Observe agent work](observe-agent-work.md).

## Before you start

Have a running Holon daemon, access to its configuration and control token, and
a checkout of this repository for the sample assets. Install the OpenTelemetry
Collector (`otelcol-contrib`), Prometheus, and Grafana if you intend to use all
three. The commands below run from the repository root.

Plan a restart window before changing daemon configuration. Keep tokens in a
credential store or mounted secret file. The example Collector listens on
`0.0.0.0:4318`; bind it to loopback for a local test or restrict network access
before starting it.

## 1. Start a test collector

Use a deployment copy of the sample configuration, adjusting its bind address
for your environment. Replace the path below with that prepared copy:

```bash
otelcol-contrib --config /path/to/prepared/otel-collector.yaml
```

The sample accepts OTLP/HTTP and sends traces to the Collector's debug exporter.
For a production deployment, replace that exporter with your trace backend.

## 2. Enable Holon's exporter

Merge the following settings into the existing `runtime.observability.otlp`
object in `<HOLON_HOME>/config.json`. Preserve unrelated configuration:

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

For a collector that requires a bearer token:

```bash
printf '%s' "$OTLP_TOKEN" \
  | holon config credentials set --kind bearer_token --stdin otlp-collector
```

Add `"credential_profile": "otlp-collector"` to the OTLP object. Do not also set
an `authorization` header in `headers`. Check configuration, then restart during
the agreed window:

```bash
holon config doctor
holon daemon restart
holon daemon status
```

Submit a small authorized test request to an existing agent:

```bash
holon prompt --agent builder "Reply with a brief acknowledgement. Do not run commands or change files."
holon tail --agent builder --limit 5
```

Confirm both the agent's response and received spans in Collector output. A
report appearing in Holon does not establish that trace export worked.

## 3. Check protected metrics

Use your daemon's actual HTTP address:

```bash
curl --fail --silent --show-error \
  -H "Authorization: Bearer $HOLON_CONTROL_TOKEN" \
  http://127.0.0.1:7878/api/control/runtime/metrics
```

Expect OpenMetrics text containing `holon_process_uptime_seconds` and ending
with `# EOF`. Check exporter queue, failure, and drop counters after the test
request. For an authenticated remote deployment, also confirm that an
unauthenticated request is rejected.

## 4. Add Prometheus and Grafana

Prepare deployment copies of:

- `docs/website/assets/observability/prometheus.yaml`
- `docs/website/assets/observability/holon-alerts.yaml`
- `docs/website/assets/observability/grafana-dashboard.json`

The Prometheus sample expects the control token at
`/run/secrets/holon-control-token`, alert rules at
`/etc/prometheus/holon-alerts.yaml`, and Holon at `127.0.0.1:7878`. Mount or adjust
those paths and address for your deployment. In containers, localhost refers to
that container, so set the target to the reachable Holon service.

```bash
prometheus --config.file=/path/to/prepared/prometheus.yaml
```

Use the prepared configuration path, not the unmodified repository sample.
Check that the Holon scrape target is healthy and that
`holon_process_uptime_seconds` returns samples. Import the dashboard into Grafana,
select the Prometheus data source, and verify that uptime and exporter panels
show data. Confirm the alert rules loaded successfully. Tune thresholds after
observing a representative workload.

## 5. Diagnose a missing signal

| Symptom | Next check |
|---|---|
| No metrics response | Address, network path, authentication, and daemon status |
| Metrics work but no exported spans | OTLP endpoint, collector receiver, credentials, and `holon_otlp_exporter_failed_batches_total` |
| Export queue or drops increase | Collector throughput and `holon_otlp_exporter_queue_depth` / `holon_otlp_exporter_dropped_spans_total` |
| Missing locally retained traces | Retention/sampling policy and diagnostics writer failure/drop counters |
| Prometheus works but dashboard is empty | Data source selection, query time range, and sample timestamps |
| Agent task failed but telemetry is healthy | The task's report and output, using [Troubleshooting](troubleshooting.md) |

For an incident with a message or turn ID, search retained traces:

```bash
holon debug trace --search MESSAGE_OR_TURN_ID
holon debug trace TRACE_ID
```

Replace the placeholders with actual IDs. Local retention is independent of
OTLP export, and sampling means not every trace is retained. Avoid restarting
only to reset counters: capture before/after snapshots and preserve incident
evidence first.

## Acceptance and rollback

The setup is verified when a test turn produces an agent report, Collector
spans, readable protected metrics, and dashboard samples. Record any component
you did not configure or test.

If export must be disabled, set `runtime.observability.otlp.enabled` to `false`
and restart in the agreed window. Collector failure should not block task
execution; diagnose it as a telemetry failure rather than rerunning business
actions automatically.

See the [Observability reference](../reference/observability.md) for configuration
fields, endpoint contracts, and performance snapshot fields.
