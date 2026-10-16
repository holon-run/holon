---
title: 可观测性
summary: Trace 导出、受保护的指标端点、OTLP 字段定义以及运行时性能快照契约。
order: 40
---

# 可观测性

有关部署与经过验证的测试流程，请参阅[配置可观测性](../guides/configure-observability.md)。有关单个任务的进展与结果排查，请参阅[观察 Agent 工作](../guides/observe-agent-work.md)。

Holon 将遥测开销严格限制在有界范围内：

- 已完成的 span 进入内存中的最近循环缓冲区；
- 部分选定的慢执行、失败、取消或采样的 trace 会保留在 `diagnostics.sqlite` 中；
- 可选的 OTLP 导出使用有界的非阻塞队列；
- OpenMetrics 端点暴露一组固定的、无高基数 label 的 `holon_*` 序列。

导出器或采集器故障不会阻塞 Turn 轮次推进、工具执行、状态持久化或结果交付。导出器的丢弃与失败计数器可用于识别降级的遥测状态。

## 用 OTLP/HTTP 导出 trace

配置项位于 `<HOLON_HOME>/config.json` 中的 `runtime.observability.otlp`：

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

| 字段 | 契约说明 |
|---|---|
| `enabled` | 启用可选导出器，默认禁用 |
| `endpoint` | 显式的 HTTP 或 HTTPS trace 端点，启用时必填 |
| `queue_capacity` | 非阻塞导出队列的容量上限 |
| `batch_size` | 每次导出的批次大小 |
| `batch_interval_ms` | 批次间隔时间（毫秒） |
| `timeout_ms` | 导出请求超时时间（毫秒） |
| `credential_profile` | 用于 Bearer 鉴权的可选凭据存储配置文件 |
| `headers` | 静态非机密请求头；不要将 `authorization` 请求头与 `credential_profile` 混用 |

修改 OTLP 设置后需重启守护进程。采集器 Token 请通过 `holon config credentials set --kind bearer_token --stdin PROFILE` 存储；在配置中引用 profile，不要直接写入明文 Token。

基准 Collector 配置位于 `docs/website/assets/observability/otel-collector.yaml`，监听端口 `4318` 接收 OTLP/HTTP JSON 并使用 debug 导出器。它默认监听在 `0.0.0.0`，实际部署时请限制其绑定地址或网络访问权限。

## 抓取受保护的 OpenMetrics

| 属性 | 规格契约 |
|---|---|
| 路由 | `GET /api/control/runtime/metrics` |
| 鉴权 | 与其他 `/api/control/*` 路由相同的控制平面鉴权 |
| Content-Type | `application/openmetrics-text; version=1.0.0; charset=utf-8` |
| 结束标记 | `# EOF` |
| 基数 | 固定序列数量上限，无动态 label |

Trace ID、Agent ID、模型名称、URL、错误文本等高基数内容绝不会成为指标 label。控制端点应保留在回环接口（127.0.0.1）或受保护的私有网络中。请勿在仪表盘 JSON 或抓取配置文本中硬编码 Bearer Token。

基准配置 `docs/website/assets/observability/prometheus.yaml` 从 `/run/secrets/holon-control-token` 读取 Token，抓取 `127.0.0.1:7878`，并从 `/etc/prometheus/holon-alerts.yaml` 加载规则。这些是需要根据实际环境配置的部署路径，并非 Holon 自动安装的文件。

## 安装仪表盘和告警

[配置指南](../guides/configure-observability.md#4-添加-prometheus-与-grafana)提供了安装与验证步骤。配套维护的资产包括：

| 资产 | 包含内容 |
|---|---|
| `docs/website/assets/observability/grafana-dashboard.json` | 运行时间、Turn 延迟（p50/p95/p99）、投影门控压力、诊断写入器队列/丢弃/失败以及 OTLP 队列/导出/丢弃/失败面板 |
| `docs/website/assets/observability/holon-alerts.yaml` | Prometheus 基准告警规则 |

告警阈值只是推荐起点，并非普适的 SLA 目标。请根据实际观察到的工作负载调整 Turn 延迟和告警持续时间。

## 诊断遥测故障

| 信号 | 识别的故障场景 |
|---|---|
| `holon_process_uptime_seconds` | 抓取目标是否可达以及进程运行时长 |
| `holon_otlp_exporter_failed_batches_total` | 采集器连接、凭据、TLS 或端点故障 |
| `holon_otlp_exporter_dropped_spans_total` | 队列压力下丢失的导出遥测数据 |
| `holon_otlp_exporter_queue_depth` | 待导出的积压队列深度 |
| `holon_diagnostics_writer_dropped_traces_total` | 丢失的本地保留任务 |
| `holon_diagnostics_writer_failures_total` | 本地诊断持久化写入失败 |

本地 Trace 保留与 OTLP 导出彼此独立。可以使用 `holon debug trace --search MESSAGE_OR_TURN_ID` 搜索，再用 `holon debug trace TRACE_ID` 查看匹配的保留 Trace。是否存在记录取决于采样和保留策略。在修改配置或重启遥测服务前，请参阅[排查流程](../guides/configure-observability.md#5-排查缺失的遥测信号)。

## 运行时性能快照

`GET /api/control/runtime/performance` 返回进程生命周期内性能计数器的受保护 JSON 快照。重启守护进程会重置这些计数器；做快照比对时应在同一个进程生命周期内进行。

### 指标分组

| 分组 | 典型示例 |
|---|---|
| `turn.*` | `turn.total`, `turn.context_build`, `turn.provider_round`, `turn.tool_execution`, `turn.cleanup` |
| `provider.*` | `provider.request_build`, `provider.round_total`, `provider.retry` |
| `tool.execution` | 工具执行累计耗时 |
| `storage.*` | `storage.append_event`, `storage.persist_state` |
| `projection.agent_state.*` | Task、Timer、WorkItem、Wait 以及外部触发器的投影耗时 |
| `projection.agents_list` | Agent 列表投影耗时 |
| `http.*` | 按路由划分的 HTTP 处理耗时 |
| `scheduler.*` | `scheduler.poll.all`, `.message`, `.idle`, `.stopped` 及其他调度阶段耗时 |

快照中还包含其他子系统的内部计数器。上表展示了主要的指标族分类，并非穷尽列表。

### 指标字段

| 字段 | 含义说明 |
|---|---|
| `name` | 指标标识符 |
| `count` | 记录的调用次数 |
| `total_ms` | 累计耗时毫秒数 |
| `max_ms` | 最大记录耗时 |
| `avg_ms` | 平均耗时 |
| `p50_ms`, `p95_ms`, `p99_ms` | 基于直方图采样的耗时分位数（p50/p95/p99） |
| `total_bytes`, `avg_bytes` | 对于记录字节大小的指标，提供总字节数与平均字节数 |

对于可复现的操作，建议在操作前后分别抓取快照进行比对，而不是专门重启服务来清零指标。当没有 Agent 拥有可运行任务时，较高的空闲轮询计数（idle poll）属于正常现象。

## 相关参考与概念

- [HTTP 控制平面参考](http-control-plane.md)
- [配置参考](configuration.md)
- [配置可观测性](../guides/configure-observability.md)
