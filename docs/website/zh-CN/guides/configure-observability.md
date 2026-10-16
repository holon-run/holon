---
title: 配置可观测性
summary: 导出追踪 Trace、抓取受保护的指标端点，并将遥测通道与业务执行独立核验。
order: 270
---

# 配置可观测性

本指南介绍如何对跨任务运行的 Holon 守护进程进行全局监控。如果你只需要了解某项具体目标的推进进度或最终成果，请先参阅[观察 Agent 工作](observe-agent-work.md)。

## 前置条件

- 正常运行中的 Holon 守护进程、修改其配置的权限以及控制平面访问 Token。
- 检出当前代码仓库以获取示例配置文件。
- 若打算完整使用三件套，请先安装 OpenTelemetry Collector (`otelcol-contrib`)、Prometheus 与 Grafana。
- 以下命令均在仓库根目录下运行。

修改守护进程配置前先安排好重启窗口。将 Token 保存在凭据存储或挂载的 Secret 文件中。示例中的 Collector 监听在 `0.0.0.0:4318`；若在本地测试请绑定至 Loopback，或在启动前限制其网络访问范围。

## 1. 启动测试 Collector

使用示例配置的副本，并根据需要调整绑定地址：

```bash
otelcol-contrib --config docs/website/assets/observability/otel-collector.yaml
```

该示例接收端口 `4318` 上的 OTLP/HTTP JSON 请求，并将收到的 Trace 打印至调试输出。生产环境中应替换为真实的后端导出器。

## 2. 启用守护进程的 Trace 导出

在 `<HOLON_HOME>/config.json` 中配置导出器：

```json
{
  "observability": {
    "traces": {
      "exporter": "otlp_http",
      "endpoint": "http://127.0.0.1:4318/v1/traces",
      "protocol": "http_json",
      "sample_ratio": 1.0
    }
  }
}
```

安全重启守护进程，并检查导出器启动日志：

```bash
holon daemon status
holon daemon logs | grep -i otel
```

## 3. 运行测试调用并验证 Trace

向测试 Agent 发送一个简单的请求：

```bash
holon run --agent observer "用一句话解释什么是追踪（Trace）。不要修改任何文件。"
```

检查 Collector 日志。你应该能看到代表该次交互轮次的 Span 以及对应的工具调用记录。Trace 的成功导出仅证明遥测链路通畅，并不等于该次业务目标的成果已经得到人工验收。

## 4. 抓取受保护的指标端点（Metrics）

Holon 在管理端口上暴露 OpenMetrics 指标端点。该端点需要携带 Bearer Token 认证。

在 Prometheus 中配置抓取作业：

```yaml
scrape_configs:
  - job_name: "holon"
    scrape_interval: 15s
    scheme: http
    static_configs:
      - targets: ["127.0.0.1:7878"]
    metrics_path: "/metrics"
    authorization:
      type: Bearer
      credentials: "YOUR_HOLON_CONTROL_TOKEN"
```

启动 Prometheus 并验证指标抓取状态：

```bash
curl -H "Authorization: Bearer $HOLON_CONTROL_TOKEN" http://127.0.0.1:7878/metrics | grep holon_
```

输出应包含以 `holon_` 为前缀的指标系列，例如请求计数器、活跃任务数与导出器状态。

## 5. 配置仪表盘与告警

将仓库提供的 Grafana 示例仪表盘（`docs/website/assets/observability/grafana-dashboard.json`）导入你的监控系统。

重点监控指标包括：
- `holon_exporter_dropped_spans_total`：由于网络或后端故障丢弃的 Span 总数。
- `holon_active_tasks`：当前处于运行或排队状态的任务数。
- 守护进程的内存与进程健康状况。

遥测链路的失败不会阻断业务交互，但应及时排查告警以防监控盲区。

## 另见

- [可观测性参考](/zh-CN/reference/observability.md) — 完整的 OTLP 字段、指标项与契约说明
- [观察 Agent 工作](observe-agent-work.md) — 单个任务的状态查验
