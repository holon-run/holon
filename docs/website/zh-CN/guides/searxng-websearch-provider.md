---
title: 用自托管 SearXNG 配置 Holon WebSearch 搜索后端
summary: "使用 Docker 自托管 SearXNG 开源元搜索引擎，为 Holon 提供免云端凭据的 WebSearch 本地搜索后端：容器部署调优、配置接入与端到端验证排错。"
order: 211
---

# 用自托管 SearXNG 配置 Holon WebSearch 搜索后端

将自托管的 SearXNG 开源元搜索引擎接入 Holon，作为 `WebSearch` 工具的本地数据面后端。通过自建搜索实例，Agent 在执行调研和信息检索时无需配置第三方商业搜索 API 凭据，同时有效避免搜索词向外部云端分析服务出域，适合对数据隐私敏感或需要严格控制搜索开销的部署环境。

本文基于真实账号级自托管部署与隔离实例端到端联调，提供从单容器部署调优、Holon provider 接入、正反向实测证据到故障排错的完整指引。如需了解网络工具的基本取舍与安全边界，请参阅[获取与搜索网络内容](./use-web-tools.md)与[Web 工具参考](../reference/web-tools.md)。

## 前置条件

- 一台安装并运行 Docker 的宿主机。
- Holon 0.59.0 或更高版本（内置 `kind = "searxng"` Web 提供商支持）。
- SearXNG 官方 Docker 镜像（本文实测版本：`searxng/searxng:2026.11.9-57ff753ad`）。
- 宿主机具备正常的出网访问能力（用于 SearXNG 向上游公网引擎请求检索）。

## 第 1 步：部署 SearXNG（Docker）

### 1. 准备配置文件 `settings.yml`

创建本地目录并编写最小可用配置。显式开启 JSON API 格式，并按网络可达性精简上游引擎：

```yaml
# settings.yml
# 仅本机回环访问；单用户自用场景关闭 limiter
use_default_settings:
  engines:
    keep_only:
      - bing
      - bing news
      - baidu

engines:
  - name: bing
    disabled: false
  - name: bing news
    disabled: false
  - name: baidu
    disabled: false

server:
  secret_key: "<openssl rand -hex 24 生成随机字符串>"
  limiter: false
  image_proxy: false

search:
  formats:
    - html
    - json        # 必须显式包含：Holon searxng provider 依赖 format=json；缺省时 API 请求返回 HTTP 403
  timeout: 8.0
  # 缩短超时惩罚时长，避免网络偶发抖动时唯一可用引擎被长期禁用
  ban_time_on_fail: 5
  max_ban_time_on_fail: 30
```

部署要点与避坑提示：

- **`search.formats` 必须包含 `json`**：SearXNG 默认未开启 JSON API。缺失此项将导致所有 JSON API 请求返回 HTTP 403 HTML 错误页，这是接入失败最常见的原因。
- **精简活跃引擎**：通过 `use_default_settings.engines.keep_only` 筛选可靠引擎，并在 `engines:` 列表中覆盖启用状态（`disabled: false`）。SearXNG 内置的大量引擎在数据中心或特定网络环境下可达性差异极大，全量开启会显著拉长请求等待时间，并产生过多无响应引擎报错。
- **避免未配置凭据的引擎报错**：带有 `inactive: true` 标记的引擎（如 marginalia）需要单独配置 `api_key` 等必填属性才能加载，本文采用零外部凭据方案，直接忽略此类引擎即可。

### 2. 启动容器

使用固定版本镜像、host 网络模式并严格绑定回环地址启动容器：

```bash
docker run -d --name searxng \
  --network host \
  -e GRANIAN_HOST=127.0.0.1 \
  -e GRANIAN_PORT=8888 \
  -v "$PWD/settings.yml:/etc/searxng/settings.yml:ro" \
  searxng/searxng:2026.11.9-57ff753ad
```

网络与安全关键细节：

- **为什么推荐使用 host 网络模式**：实测表明，在部分数据中心网络环境下，Docker 默认的 bridge NAT 网络栈会导致部分上游搜索引擎（如 Bing）在容器内持续超时，而宿主机直连却完全正常（延迟 1.1s~1.9s）。使用 host 网络让 SearXNG 直接复用宿主网络栈，可有效消除 bridge 丢包导致的“服务健康但搜不到结果”的隐蔽网络问题。
- **严格限制监听范围（防范未授权访问）**：host 模式下容器应用默认可能监听 `*:8888`。由于当前镜像使用 granian 作为 ASGI 入口，必须显式传递 `-e GRANIAN_HOST=127.0.0.1` 强制绑定到回环地址（此时传统的 `SEARXNG_BIND_ADDRESS` 环境变量不生效）。启动后可在宿主机执行 `ss -ltn | grep 8888` 确认仅监听 `127.0.0.1:8888`。若必须使用 bridge 桥接网络，可通过 `-p 127.0.0.1:8888:8088` 限制暴露面，但需自行排查并确认上游网络无丢包。

启动后执行冒烟测试验证 JSON API 输出：

```bash
curl -s "http://127.0.0.1:8888/search?q=test&format=json" | grep -o '"results":\['
```

若返回匹配则说明 SearXNG JSON 接口运行正常。

## 第 2 步：配置 Holon

可以通过 Holon CLI 写入配置，也可以在对话中直接让 Agent 完成配置检查。

### 方式 A：使用 Holon CLI 配置

在终端中依次执行以下命令：

```bash
# 注册 searxng 提供商端点
holon config set web.providers.searxng.kind searxng
holon config set web.providers.searxng.base_url "http://127.0.0.1:8888"

# 将 WebSearch 默认搜索服务商切换为 searxng
holon config set web.search.enabled true
holon config set web.search.provider searxng
```

### 方式 B：直接编辑配置文件

在 Holon 的配置文件（`config.json`）的 `web` 节点中追加对应的提供商与搜索路由：

```json
{
  "web": {
    "search": {
      "enabled": true,
      "provider": "searxng",
      "mode": "single"
    },
    "providers": {
      "searxng": {
        "kind": "searxng",
        "base_url": "http://127.0.0.1:8888"
      }
    }
  }
}
```

配置特性：

- **零凭据接入**：searxng provider 不需要设置 `credential_profile` 或任何 API Key。
- **路径前缀支持**：`base_url` 支持带反向代理路径前缀（如 `https://internal.example.com/searxng/`），Holon 会自动拼接 `/search` 并保留前缀。
- **一致的结果归一化**：Agent 调用 `WebSearch` 时，搜索结果数量由 Holon 经 `max_results` 参数统一标准化（默认每次 5 条），返回的标题、URL、摘要及引用结构与云端商业提供商完全一致。

## 第 3 步：端到端验证与实测证据

### 正向功能验证

在与 Agent 的对话中发起检索任务：

```text
使用 WebSearch 搜索 "Holon runtime agent"，并总结前 3 条结果的核心内容与来源 URL。
```

Agent 将自动调用 `WebSearch { query: "Holon runtime agent", max_results: 5 }`。测试实测数据记录如下：

- **JSON 接口冒烟**：分别检索开源项目、技术文档和近期新闻三类关键词，各返回 10 条结构化数据，响应耗时约 0.6s。
- **隔离实例端到端路由**：在全新独立的 `HOLON_HOME` 实例中配置 `kind = "searxng"`，Agent 发起搜索后正确路由到本地 SearXNG 实例，返回 5 条包含权威仓库与官方文档的有效条目。
- **连续多领域检索稳定性**：连续执行 8 次不同领域的关键词搜索，8 次均稳定返回 10~11 条原始聚合结果（上游个别引擎如百度触发安全验证码时会被置为挂起，但其余健康引擎正常兜底，不影响整体交付）。
- **搜索与抓取闭环（WebSearch + WebFetch）**：Agent 接着调用 `WebFetch` 读取搜索结果中的 URL，成功获取页面并提取版本号与正文，完成“检索目标 → 读取全文”闭环。
- **结果质量对比**：与商业云搜索服务商（如 Brave）对比相同关键词，自托管 SearXNG 结果覆盖面更广、来源更具多样性，完全能满足工程调研与日常文档检索需求。

### 反向异常与错误语义验证

针对异常部署场景的测试核验：

| 异常场景 | SearXNG 服务端行为 | Holon Agent 呈现与诊断 |
|---|---|---|
| `search.formats` 未配置 `json` | 返回 HTTP 403 Forbidden（HTML 页面） | 报告 `network_failed`，明确提示 `HTTP 403 Forbidden`，标记 `retryable: true` |
| `base_url` 端口未开放或地址不可达 | TCP 连接直接被拒绝（Connection refused） | 报告 `network_failed`，显示完整请求 URL，提示重试或切换其他可用服务商 |
| 所有配置的上游引擎全部超时或触发验证码 | 返回 HTTP 200 与合法空结果 `{"results": []}`，响应包含 `unresponsive_engines` 状态 | 报告 `parse_failed`（提示 "no parseable search results"） |

> **说明**：当上游引擎因网络波动导致全量超时或反爬时，SearXNG 仍返回 HTTP 200 空列表，Holon 侧将其归类为 `parse_failed` 呈现给 Agent。相关语义优化建议已记录在社区 issue [holon-run/holon#4024](https://github.com/holon-run/holon/issues/4024) 中，该现象属于外部上游网络暂时不可用，通过配置多个稳定引擎或稍后重试即可恢复。

## 第 4 步：故障排查

- **WebSearch 报错 `network_failed` 且伴随 `HTTP 403 Forbidden`**：
  SearXNG 容器中的 `settings.yml` 未启用 JSON 格式。检查 `search.formats` 下是否包含 `- json`，修改后重启容器。
- **WebSearch 报错 `network_failed` 且提示 request failed / connection refused**：
  Holon 无法连接至指定的 `base_url`。先在宿主机执行 `curl -s "http://127.0.0.1:8888/search?q=test&format=json"` 确认容器自身运行正常；再检查防火墙或网络路由。
- **WebSearch 报错 `parse_failed`，但用 curl 查询有时能正常返回结果**：
  实例健康，但当前请求瞬间所有启用的上游引擎均超时或触发了反爬验证。可在容器宿主机查看 SearXNG 响应里的 `unresponsive_engines` 字段；主要排查手段包括：容器改用 host 网络避免 NAT 丢包、在 `settings.yml` 中适当调大 `search.timeout`（如 8~10 秒），以及缩短 `ban_time_on_fail` 惩罚周期。
- **搜索结果时有时无、偶发无结果**：
  单个引擎波动叠加惩罚挂起机制导致的“空窗效应”。建议在 `keep_only` 中配置至少两个不共享底层基础设施的上游引擎（例如 Bing 与其他备用源）进行互补分流。

## 第 5 步：已知限制

- **数据中心网络下的引擎可达性差异较大**：实测发现，许多公共云服务商的机房 IP 会被部分搜索服务（如 DuckDuckGo、Brave、Startpage）在 TLS 握手阶段阻断或严格限流，百度在机房网络下也较容易触发滑块验证码。在免凭据自托管场景下，Bing 系引擎的可达性相对最高。如需接入对反爬较敏感的源，可考虑申请支持 API Key 的专用引擎服务（如 Marginalia），在 `settings.yml` 中补全凭据后启用。
- **单引擎依赖的窗口性抖动**：若仅启用单一引擎，一旦该引擎在某次查询中超时并触发短暂停用惩罚，随后的查询会在短时间内出现空结果；生产环境建议至少配置 2 个可用的独立上游引擎。
- **多语言支持依赖启用的引擎库**：Bing 等通用引擎对中英文技术内容的覆盖均较完备；若有大量特定小语种检索需求，建议有针对性地开启对应语种的区域引擎。

## 第 6 步：资源清理

完成验证或停用本地自托管搜索后，可安全清理容器及临时挂载：

```bash
# 停止并删除 SearXNG 容器
docker rm -f searxng

# 清理未使用的 Docker 匿名卷（如有）
docker volume ls -q | grep searxng | xargs -r docker volume rm
```

若需切回其他提供商，只需在 Holon 中执行对应更新即可：

```bash
holon config set web.search.provider duckduckgo
```

## 另见

- [获取与搜索网络内容指南](./use-web-tools.md)：在检索与抓取之间做取舍，以及处理不可信外部内容
- [Web 工具参考](../reference/web-tools.md)：WebFetch、WebSearch 与 XSearch 的详细参数与服务商清册
- [连接外部事件指南](./connect-external-events.md)：利用 Webhook 与外部回调唤醒 Agent
