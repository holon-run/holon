---
title: Observability
summary: Trace export, protected metrics endpoints, and configuration for OTLP, OpenMetrics, dashboards, and alerts.
order: 40
---

# Runtime observability

Holon keeps its tracing and performance hot paths bounded:

- completed spans enter an in-memory recent ring;
- selected slow, failed, cancelled, or sampled traces may be retained in
  `diagnostics.sqlite`;
- optional OTLP export uses a bounded non-blocking queue;
- OpenMetrics exposes a fixed set of label-free `holon_*` series.

Exporter or collector failure does not block turns, tools, persistence, or
delivery. Watch the exporter drop and failure counters to detect degraded
telemetry.

## Export traces with OTLP/HTTP

Start an OpenTelemetry Collector with the baseline configuration:

```bash
otelcol-contrib \
  --config docs/website/assets/observability/otel-collector.yaml
```

The example accepts OTLP/HTTP JSON on port `4318` and writes received traces
to the Collector debug exporter. Replace `debug` with the exporter for your
trace backend.

Enable Holon's exporter in `<HOLON_HOME>/config.json`:

```json
{
  "runtime": {
    "observability": {
      "otlp": {
        "enabled": true,
        "endpoint": "http://127.0.0.1:4318/v1/traces",
        "queue_capacity": 1024,
        "batch_size": 128,
        "batch_interval_ms": 1000,
        "timeout_ms": 5000
      }
    }
  }
}
```

Restart the daemon after changing OTLP settings. The exporter is disabled by
default, and an enabled exporter requires an explicit HTTP or HTTPS endpoint.

For an authenticated collector, store the token in the credential store
instead of `config.json`:

```bash
printf '%s' "$OTLP_TOKEN" \
  | holon config credentials set --kind bearer_token --stdin otlp-collector
```

Then add `"credential_profile": "otlp-collector"` to the OTLP config. Holon
adds the profile material as a bearer authorization header. Static
non-secret headers may be placed in the `headers` object; do not configure an
`authorization` header together with `credential_profile`.

## Scrape protected OpenMetrics

The endpoint is:

```text
GET /api/control/runtime/metrics
```

It uses the same control-plane bearer authentication as the other
`/api/control/*` routes and returns:

```text
application/openmetrics-text; version=1.0.0; charset=utf-8
```

For a local check:

```bash
curl --fail \
  --header "Authorization: Bearer $HOLON_CONTROL_TOKEN" \
  http://127.0.0.1:7878/api/control/runtime/metrics
```

The response has no labels, ends with `# EOF`, and has a fixed series-count
ceiling. Trace IDs, agent IDs, model names, URLs, error text, and other
high-cardinality values never become metric labels.

The baseline Prometheus scrape configuration reads the token from a mounted
secret file:

```bash
prometheus \
  --config.file=docs/website/assets/observability/prometheus.yaml
```

Copy these example files into the paths used by your deployment:

- `docs/website/assets/observability/prometheus.yaml`
- `docs/website/assets/observability/holon-alerts.yaml`

Keep the control endpoint on a loopback or private network interface. Treat
the control token as a secret and do not place it in Prometheus configuration
text or dashboard JSON.

## Install the dashboard and alerts

Import
`docs/website/assets/observability/grafana-dashboard.json` into Grafana and
select the Prometheus data source. The baseline panels cover:

- process uptime;
- turn p50/p95/p99 latency;
- projection-gate pressure;
- diagnostics writer queue, drops, and failures;
- OTLP exporter queue, exported spans, drops, and failed batches.

Load `holon-alerts.yaml` as a Prometheus rule file. Its thresholds are
conservative starting points, not universal service-level objectives. Tune
turn latency and alert duration after observing a representative workload.

## Diagnose telemetry failures

Use this order:

1. Check `holon_process_uptime_seconds` to distinguish scrape failure from a
   stalled subsystem.
2. Check `holon_otlp_exporter_failed_batches_total` for collector,
   credential, TLS, or endpoint failures.
3. Check `holon_otlp_exporter_dropped_spans_total` and
   `holon_otlp_exporter_queue_depth` for sustained backpressure.
4. Check `holon_diagnostics_writer_dropped_traces_total` and
   `holon_diagnostics_writer_failures_total` for local diagnostic retention
   problems.
5. Correlate the incident with a retained trace:

   ```bash
   holon debug trace --search <message-or-turn-id>
   holon debug trace <trace-id>
   ```

An OTLP failure does not imply that local trace search failed: retained traces
remain in the independent diagnostics database according to the configured
sampling and retention policy.
