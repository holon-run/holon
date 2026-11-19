---
title: "在离线与自托管环境中运行私有 Agent：从 Ollama 到私有搜索与本地工具链"
summary: "把 Holon 运行时部署在纯断网离线或内网自托管环境中：锁定本地 Ollama/vLLM 模型与禁用云端回退，配置 SearXNG 私有搜索或命令型本地检索、WebFetch 内网白名单、本机工具链边界，并在对话中完成验证。"
order: 135
---

# 在离线与自托管环境中运行私有 Agent：从 Ollama 到私有搜索与本地工具链

本指南用于将 Holon 配置为不依赖公有云大模型或云端 SaaS 的私有运行时。它覆盖两类常见隔离场景：

- **纯离线（Air-gapped）环境**：宿主机完全断网或禁止任何外部访问。模型推理、本地工具执行、工作区读写与检索全部在本机闭环。
- **内网自托管（Self-hosted Intranet）环境**：允许访问受控的局域网基础设施（如自托管 Ollama/vLLM、SearXNG、GitLab CE、GlitchTip），但严禁数据流向公有云大模型或公网域名。

本文先完成宿主机上的守护进程基线配置（模型锁定与网络策略），随后全部通过 [Web GUI](./use-web-gui.md) 的自然语言对话让 Agent 挂载本地代码库、写入 `AGENTS.md` 边界规则，并逐项核验离线与自托管状态。

## 前置条件

- 已安装 Holon 运行时，且拥有修改 `<HOLON_HOME>/config.json` 与重启守护进程的权限。
- **本地模型服务已就绪**：
  - 本机或内网运行 [Ollama](https://ollama.com/)（默认 `http://127.0.0.1:11434`）或兼容 OpenAI Chat Completions 协议的 [vLLM](https://docs.vllm.ai/)（默认 `http://127.0.0.1:8000/v1`），且已提前拉取所需权重。
  - 若你在 Apple Silicon 或 64GB 统一内存桌面上运行多轮编码 Agent，模型与推理后端选型可参考[《本地大模型能跑通多轮编码 Agent 吗？GGUF、MoE 轮次膨胀与 Prompt Cache 实测》](../blog/local-model-agent-benchmark.md)（实测推荐 `qwen3.8:27b-gguf` 或 `gemma4:26b`，并避免将 Ollama GGUF 的 `Modelfile` 通过管道 `ollama create -f -` 导入）。
- **可选内网依赖（仅自托管场景）**：内网已部署 Docker 宿主（如需自托管 SearXNG）、内部 Git 仓库或本地文档索引。

---

## 第 1 步：锁定本地模型并禁止云端回退

在纯私有环境中，仅仅把 `model.default` 设为本地模型还不够。如果环境变量里曾配置过云端 API Key，且开启了默认自动回退，主模型遇到超时或异常时仍可能把上下文发往云端备用服务商。

在启动或重启 Holon 守护进程前，先在宿主机锁定本地模型并关闭自动回退：

### 方案 A：使用本机 Ollama（无需 API Key）

```bash
holon config set model.default "ollama@default/qwen3.8:27b-gguf"
holon config set runtime.disable_provider_fallback true
```

若使用标准 Ollama 库中的 GGUF 标签（而非默认 MLX 变体），也可填写 `ollama@default/qwen3.8:27b-q4_K_M` 或 `ollama@default/gemma4:26b`。

### 方案 B：使用本机或内网 vLLM（无需 API Key）

```bash
# 若 vLLM 运行在本机默认端口 http://127.0.0.1:8000/v1
holon config set model.default "vllm@default/<your-served-model>"
holon config set runtime.disable_provider_fallback true
```

如果 Ollama 或 vLLM 部署在内网另一台 GPU 服务器上，或 Holon 运行在容器内需要访问宿主机，请显式更新对应 Provider 的 `base_url`：

```bash
# 示例：将 Ollama 指向内网 GPU 主机或宿主机网关
holon config providers set ollama --transport anthropic_messages --base-url "http://10.0.10.20:11434" --credential-source none --credential-kind none
```

> **提示**：不要配置云端 `vision.default`、`image_generation.default` 或 `x_search`。若完全不保留云端凭据，先清理环境中遗留的 `OPENAI_API_KEY`、`ANTHROPIC_API_KEY`、`DEEPSEEK_API_KEY`、`XAI_API_KEY` 等变量，再启动守护进程。

---

## 第 2 步：按隔离级别配置网络与搜索策略

Holon 的 `WebFetch`、`WebSearch` 和 `XSearch` 均受运行时配置门禁控制。请按你所处的隔离级别二选一。

### 模式 A：纯离线（Air-gapped）—— 彻底阻断外网工具

在完全断网环境，直接关闭内置的公网抓取与搜索工具，防止 Agent 在每轮尝试外网调用时白白等待超时：

```bash
holon config set web.fetch.enabled false
holon config set web.search.enabled false
holon config set x_search.enabled false
```

如果仍希望 Agent 通过 `WebSearch` 检索本地离线文档库（例如本地 Markdown Wiki、离线 RFC 镜像或内部索引），可以保留 `web.search.enabled true`，并接入 `kind=command` 本地命令搜索提供商，无需任何 HTTP 请求：

```json
{
  "web": {
    "search": {
      "enabled": true,
      "provider": "local_docs",
      "mode": "single"
    },
    "providers": {
      "local_docs": {
        "kind": "command",
        "command": {
          "argv": ["/usr/local/bin/search-local-docs", "--query", "{{query}}", "--limit", "{{max_results}}"]
        },
        "output": {
          "format": "json",
          "mapping": {
            "title": "$.results[*].title",
            "url": "$.results[*].url",
            "snippet": "$.results[*].snippet"
          }
        }
      }
    }
  }
}
```

### 模式 B：内网自托管 —— 私有搜索与内网白名单

在内网自托管环境，既要屏蔽公网出站，又要让 Agent 访问内网服务（例如自托管 SearXNG、GitLab CE、GlitchTip 或内部文档站）：

1. **接入自托管 SearXNG 作为 `WebSearch` 后端**：
   按[《用自托管 SearXNG 配置 Holon WebSearch 搜索后端》](./searxng-websearch-provider.md)部署单容器 SearXNG 后，将其设为默认提供商和首选自动候选：

   ```bash
   holon config set web.providers.searxng.kind searxng
   holon config set web.providers.searxng.base_url "http://127.0.0.1:8888"
   holon config set web.search.enabled true
   holon config set web.search.provider searxng
   holon config set web.search.providers '["searxng"]'
   holon config set web.search.mode single
   holon config set web.search.max_provider_attempts 1
   holon config set web.search.builtin_provider.enabled false
   holon config set x_search.enabled false
   ```

   尝试次数上限也适用于显式聚合，但不是提供商白名单。请用网络出站控制确保查询不离开内网。

2. **用 `web.fetch.allowed_hosts` 锁定 `WebFetch` 允许范围**：
   Holon 的 `WebFetch` 默认出于 SSRF 防护会拦截回环地址（`127.0.0.1`、`[::1]`）和私网地址（`10.0.0.0/8`、`192.168.0.0/16` 等）。一旦设置了非空的 `web.fetch.allowed_hosts`，它同时起到双重作用：**既显式放行列表中的内网主机与端口，又拒绝访问列表之外的所有公网域名**：

   ```bash
   holon config set web.fetch.enabled true
   holon config set web.fetch.allowed_hosts '["127.0.0.1:8080", "git.internal.corp", "docs.internal.corp"]'
   ```

   云厂商元数据地址（如 `169.254.169.254`）即使被写进列表也仍会被运行时强制拦截。

---

## 第 3 步：收口决策子系统与可观测性

1. **咨询决策子系统（`AdvisoryDecision`）**：
   如果不需要第二意见工具，保持关闭或确认 `decision.enabled` 为 `false`。若需在纯离线环境使用，只允许启用本地 ONNX（`local-onnx`），且必须使用预先拷贝到磁盘的 `decision.local_onnx.model_dir`，不要依赖运行时的在线预设下载：

   ```bash
   holon config unset decision.model
   holon config set decision.local_onnx.model_dir "/opt/holon/models/jev-selector-q4f16"
   holon config set decision.local_onnx.enabled true
   holon config set decision.enabled true
   ```

   详细参数请参阅[《配置咨询决策子系统》](./configure-decision-subsystem.md)。

2. **可观测性（OpenTelemetry）**：
   Holon 默认关闭 OTLP Trace 导出（`runtime.observability.otlp.enabled = false`）。内网自托管如需集中观测，将 `endpoint` 指向本机或内网 Collector（例如 `http://127.0.0.1:4318/v1/traces`），详见[《配置可观测性》](./configure-observability.md)。

配置完成后重启 Holon 守护进程，运行一次健康检查确认配置生效：

```bash
holon config doctor
```

---

## 第 4 步：在对话中挂载工作区与记录本地工具链边界

守护进程运行起来后，后续的日常项目接入、隔离边界与工具规范无需再切回终端手改文件，直接在 [Web GUI](./use-web-gui.md) 中用自然语言指示你的 Agent 完成即可。

### 1. 挂载本地仓库并要求隔离在 Worktree 中修改

在对话中告诉 Agent：

> “请把本地仓库 `/srv/repos/internal-service` 挂载为工作区。后续所有代码改动与测试都基于 `main` 创建独立的 Git Worktree 进行，不要直接改主检出目录。”

Agent 会自动调用 `AttachWorkspace` 与 `CreateWorktree` 建立隔离目录。工作区权限与安全清理机制详见[《在项目工作区与 Git Worktree 中工作》](./work-with-workspaces.md)。

### 2. 让 Agent 把离线/内网工具链规范写入 `AGENTS.md`

对于跨会话都要遵守的工具链约束（例如依赖源、离线缓存、禁止外网包下载），让 Agent 直接固化到其长期行为契约 `AGENTS.md` 中：

> “请把以下工作规则写入你的 `AGENTS.md` 并长期遵守：
> 1. 当前处于离线/内网自托管环境，禁止在命令行执行任何访问公网仓库的 `curl`、`git clone`、`npm install`、`pip install` 或 `cargo` 在线拉取；
> 2. 构建与测试必须使用本机已缓存依赖与离线参数（例如 `cargo test --offline`、`npm ci --offline`）；
> 3. 所有代码输出和交付验证必须只引用本地文件与测试退出码，不得依赖外部网络服务。”

Agent 会自动维护 `agent_home/AGENTS.md`（或项目仓库根目录的 `AGENTS.md`），后续每次唤醒都会强制加载这条规则。

### 3. （可选）按需组合内网自托管周边服务

如果你的团队搭建了完整内网自托管研发闭环，还可以按需将 Holon 与以下开源组件串联：

- **团队内部单点登录**：使用[自托管 GitLab CE](./gitlab-ce-oidc-authentication.md)、[Keycloak](./keycloak-oidc-authentication.md) 或 [Casdoor](./casdoor-oidc-authentication.md) 作为 OIDC 身份提供商。
- **内网错误与变更自动唤醒**：通过[自托管 GlitchTip](./glitchtip-error-alerts.md) 在异常告警时唤醒 Agent，或用[自托管 changedetection.io](./changedetection-webhook-setup.md) 监控内部网页变更。

---

## 第 5 步：在对话中验收离线与自托管边界

在正式交办生产任务前，直接在 Web GUI 中发一条自检提示词，同时验证**模型路由、网络拦截与本地工具执行**：

> “请执行一次私有环境自检，不要修改任何业务代码：
> 1. 汇报你当前生效的 `model.requested_model`、`model.active_model` 与 `model.fallback_active`；
> 2. 尝试用 `WebFetch` 访问 `https://example.com`，汇报是否按预期被拒绝以及具体报错；
> 3. 如果配置了内网服务或本地搜索，验证一次预期内的调用；最后在当前工作区运行一次最小只读测试命令并汇报结果。”

核对 Agent 返回的验收证据：

1. **模型路由零外发**：`model.active_model` 为你指定的本地 `ollama@default/...` 或 `vllm@default/...`，且 `model.fallback_active` 为 `false`。
2. **公网拦截生效**：
   - 纯离线模式下，`WebFetch` 与 `WebSearch` 均无法访问公网（工具被禁用或返回策略拒绝）。
   - 内网白名单模式下，访问 `https://example.com` 明确返回 `network_denied`（提示不在 `web.fetch.allowed_hosts` 中），而访问白名单内的内网域名或端口可正常返回。
3. **本地工具链与收据可审计**：在 Web GUI 的任务详情与工具执行记录中可以看到真实的本地命令退出码与产物路径（参阅[《观察 Agent 工作》](./observe-agent-work.md)）。

---

## 常见故障排查

- **`WebFetch` 访问本机或内网服务被拒（`network_denied`）**：
  默认策略会拦截回环和私网 IP。检查 `web.fetch.allowed_hosts` 是否包含精确的主机名或 `IP:端口`（例如 `"127.0.0.1:8080"` 或 `"git.internal.corp"`）。修改后重启守护进程生效。
- **本机 Ollama 已启动但 Holon 报连接失败**：
  先在宿主机运行 `curl http://127.0.0.1:11434/api/tags` 确认服务监听正常。若 Holon 运行在 Docker 容器中而 Ollama 在宿主机，需将 `ollama` 的 `base_url` 设为 `http://host.docker.internal:11434`（或宿主机内网 IP）并确保 Ollama 监听在对应网卡地址。
- **本地长上下文编码任务频繁超时**：
  如果在 64GB Apple Silicon 上运行 27B 模型，检查是否误用了 Ollama 的 MLX 变体导致首轮 30k Prompt Prefill 过慢；改用 `ggml-org` `Q4_K_M` GGUF（通过磁盘 `Modelfile` 导入）并保持默认 FP16 KV Cache，具体对比数据见[本地大模型多轮实测 Blog](../blog/local-model-agent-benchmark.md)。
- **离线环境下 `AdvisoryDecision` 报预设缺失**：
  切勿在断网环境点击在线下载预设；将提前校验好的模型文件目录复制到本机，配置 `decision.local_onnx.model_dir` 并清空 `decision.model`。

---

## 另见

- [本地大模型能跑通多轮编码 Agent 吗？GGUF、MoE 轮次膨胀与 Prompt Cache 实测](../blog/local-model-agent-benchmark.md)
- [配置与切换大模型：从本地 Ollama 到多云灾备](./configure-models.md)
- [用自托管 SearXNG 配置 Holon WebSearch 搜索后端](./searxng-websearch-provider.md)
- [获取与搜索网络内容](./use-web-tools.md)
- [在项目工作区与 Git Worktree 中工作](./work-with-workspaces.md)
- [配置参考](/zh-CN/reference/configuration.md)
