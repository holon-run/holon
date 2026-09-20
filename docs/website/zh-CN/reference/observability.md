---
title: 可观测性
summary: Trace 导出、受保护的指标端点，以及 OTLP、OpenMetrics、仪表盘和告警的配置。
order: 40
---

# 可观测性

Holon 让链路追踪和性能热路径保持有界：

- 已完成的 span 进入内存中的近期环形缓冲；
- 被选中的慢、失败、取消或抽样的 trace 可能保留在 `diagnostics.sqlite`；
- 可选的 OTLP 导出使用有界非阻塞队列；
- OpenMetrics 暴露一组固定的、无标签的 `holon_*` 序列。

导出器或采集器故障不会阻塞轮次、工具、持久化或投递。关注导出器的丢弃和失败计数器，以发现遥测降级。

## 用 OTLP/HTTP 导出 trace

用基线配置启动 OpenTelemetry Collector：

```bash
otelcol-contrib \
  --config docs/website/assets/observability/otel-collector.yaml
```

该示例在 `4318` 端口接收 OTLP/HTTP JSON，并把收到的 trace 写入 Collector 的 debug 导出器。把 `debug` 换成你所用 trace 后端的导出器即可。

在 `<HOLON_HOME>/config.json` 中启用 Holon 导出器：

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

改动 OTLP 设置后重启 daemon。导出器默认禁用，启用时必须显式指定 HTTP 或 HTTPS 端点。

采集器需要鉴权时，把 token 存到凭据存储中，而不是放进 `config.json`：

```bash
printf '%s' "$OTLP_TOKEN" \
  | holon config credentials set --kind bearer_token --stdin otlp-collector
```

然后在 OTLP 配置里加上 `"credential_profile": "otlp-collector"`。Holon 会把该配置中的材料作为 bearer 授权头加上。非机密的静态 header 可以放在 `headers` 对象里；不要同时配置 `authorization` header 和 `credential_profile`。

## 抓取受保护的 OpenMetrics

端点是：

```text
GET /api/control/runtime/metrics
```

它使用与其他 `/api/control/*` 路由相同的控制平面 bearer 鉴权，返回：

```text
application/openmetrics-text; version=1.0.0; charset=utf-8
```

本地检查：

```bash
curl --fail \
  --header "Authorization: Bearer $HOLON_CONTROL_TOKEN" \
  http://127.0.0.1:7878/api/control/runtime/metrics
```

响应没有标签，以 `# EOF` 结尾，并有固定的序列数上限。trace ID、Agent ID、模型名、URL、错误文本等高基数取值绝不会成为指标标签。

基线 Prometheus 抓取配置从挂载的 secret 文件读取 token：

```bash
prometheus \
  --config.file=docs/website/assets/observability/prometheus.yaml
```

把这些示例文件复制到你的部署使用的路径：

- `docs/website/assets/observability/prometheus.yaml`
- `docs/website/assets/observability/holon-alerts.yaml`

把控制端点放在回环或私有网络接口上。把控制 token 当作机密，不要写进 Prometheus 配置文本或仪表盘 JSON。

## 安装仪表盘和告警

把 `docs/website/assets/observability/grafana-dashboard.json` 导入 Grafana，并选择 Prometheus 数据源。基线面板覆盖：

- 进程运行时间；
- 轮次 p50/p95/p99 延迟；
- 投影门压力；
- 诊断写入队列、丢弃和失败；
- OTLP 导出器队列、已导出 span、丢弃和失败批次。

把 `holon-alerts.yaml` 作为 Prometheus 规则文件加载。其中的阈值是保守的起点，不是通用的服务水平目标。观察有代表性的工作负载后，再调整轮次延迟和告警持续时间。

## 诊断遥测故障

按这个顺序排查：

1. 检查 `holon_process_uptime_seconds`，区分抓取失败和子系统停滞。
2. 检查 `holon_otlp_exporter_failed_batches_total`，排查采集器、凭据、TLS 或端点故障。
3. 检查 `holon_otlp_exporter_dropped_spans_total` 和 `holon_otlp_exporter_queue_depth`，判断是否存在持续背压。
4. 检查 `holon_diagnostics_writer_dropped_traces_total` 和 `holon_diagnostics_writer_failures_total`，排查本地诊断保留问题。
5. 把故障与保留的 trace 关联起来：

   ```bash
   holon debug trace --search <message-or-turn-id>
   holon debug trace <trace-id>
   ```

OTLP 故障并不意味着本地 trace 搜索失败：按配置的抽样和保留策略，保留的 trace 仍留在独立的诊断数据库中。

## 相关参考与概念

- [HTTP 控制平面参考](/zh-CN/reference/http-control-plane.md) — `/api/control/runtime/metrics` 端点的权威规范。
- [配置参考](/zh-CN/reference/configuration.md) — 完整的 observability 和 OTLP 配置项 schema。
