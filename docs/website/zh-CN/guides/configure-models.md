---
title: "配置与切换大模型：从本地 Ollama 到多云灾备"
summary: 从本地零成本离线运行 Ollama 到云端主流模型接入，在对话中随时自检生效模型，并配置自动多云灾备回退链保障高可用。
order: 130
---

# 配置与切换大模型：从本地 Ollama 到多云灾备

无论你是需要在本地完全离线、免 API Key 运行 Ollama（实现数据绝对隐私与零 Token 成本），还是接入云端高性能模型服务商（OpenAI、Anthropic、DeepSeek 等），亦或是配置多服务商自动灾备回退链（主模型遇限流或网络故障时自动无缝接力），Holon 的模型路由系统都能提供开箱即用的支持。

Holon 采用统一的 `provider@endpoint/model` 路由规范（例如 `ollama@default/qwen2.5-coder:latest` 或 `deepseek@default/deepseek-chat`），并在底层严格区分“请求配置的模型”与“实际运行的模型”，让你在对话中清晰掌控每一个 Agent 的真实调用情况。

## 前置条件

- 具备对 Holon 运行时的操作权限（本地终端或 [Web GUI](./use-web-gui.md)）。
- **本地模型路线**：本地已安装并运行 [Ollama](https://ollama.com/)（默认监听 `http://localhost:11434`）。
- **云端服务商路线**：拥有对应服务商账号及 API 密钥（如 DeepSeek、OpenAI、Anthropic 等）。
- 凭据安全准则：切勿将 API Key 直接粘贴到公开对话框、截图或 Issue 报告中。建议通过交互向导或环境变量注入。

## 操作步骤

<a id="local-or-custom-endpoints"></a>
### 场景一：零配置接入本地 Ollama（离线与免密钥）

如果你希望在本地完全免费且离线运行 Agent，本地安装的 Ollama 是最简单的起点：

1. **拉取并运行本地模型**：
   在终端中使用 Ollama 下载适合编程或推理的模型：
   ```bash
   ollama pull qwen2.5-coder:latest
   ```
2. **设为全局默认模型**：
   Holon 会自动扫描本机正在运行的 Ollama 服务，无需任何 API Key：
   ```bash
   holon config set model.default "ollama@default/qwen2.5-coder:latest"
   ```
3. **验证连通性**：
   运行配置诊断命令，确认本地 Ollama 端点健康可用：
   ```bash
   holon config doctor
   ```

> **本地选型与全栈私有化收口**：如果你计划在 64GB 桌面设备（如 Apple Silicon）上跑多轮编码 Agent，模型与推理后端实测对比（`llama.cpp`/GGUF vs MLX、MoE 轮次膨胀与 KV 缓存开关）请参阅[《本地大模型能跑通多轮编码 Agent 吗？》](../blog/local-model-agent-benchmark.md)。若需彻底锁定本地运行环境（禁用云端回退、阻断公网访问、接入自托管搜索与本地工具链），请直接按[《在离线与自托管环境中运行私有 Agent》](./run-offline-private-agent.md)配置。

### 场景二：快速接入云端大模型服务商

当你需要调用云端前沿推理模型时，最便捷的方式是使用交互式引导向导：

1. **运行配置向导**：
   ```bash
   holon onboard
   ```
   向导会自动列出支持的服务商列表，根据提示选择服务商（如 DeepSeek、Anthropic），通过隐密输入粘贴你的 API Key，系统会自动完成凭据 Profile 创建与服务商端点绑定。
2. **或直接通过环境变量注入**：
   在启动 Holon 守护进程的环境中直接注入对应的标准环境变量，例如：
   ```bash
   export DEEPSEEK_API_KEY="sk-..."
   export ANTHROPIC_API_KEY="sk-..."
   ```
   *注意：环境变量须在启动 Holon 守护进程（Daemon）的终端会话中设置；在独立的临时客户端窗口 export 不会影响后台已运行的服务。*
3. **查看所有可用模型编目**：
   ```bash
   holon config models list
   ```
   复制列表中支持你所需任务的标准路由标识，并设为主模型：
   ```bash
   holon config set model.default "deepseek@default/deepseek-chat"
   ```

#### GitHub Copilot（实验性）

Holon 内置了一个公开的第三方 OAuth App（opencode 项目的 App），
无需填写 client ID：

```bash
holon onboard
```

选择 `github-copilot` 并完成 OAuth 设备登录。如需使用自己的 GitHub OAuth App，
开启该 App 的设备流后配置它的 **client ID**，不要使用 App ID 或第一方编辑器的 client ID：

```bash
holon config set providers.github-copilot.auth.client_id "<your-client-id>"
```

也可以在执行登录的进程环境中设置 `GITHUB_COPILOT_CLIENT_ID`。
优先级依次为显式配置、环境变量、内置默认值；空白覆盖值会被忽略。
Holon 将凭据和 client ID 存入自己的
`github-copilot` profile，不读取 IDE 凭据，也不需要 client secret。

凭据 profile 会保留登录时记录的 client ID。如果你曾用旧版 Holon 的默认 App
登录，需要重新完成一次设备登录才能切换到当前内置 App；token 刷新会沿用原 App。

此直连 provider 属于实验性支持：GitHub 登录成功不代表 Copilot 订阅权益、
App／integration 准入或模型访问已验证。发现当前账户可用的模型后，选择返回的
路由；Holon 不提供静态模型清单。目录中声明 `/responses` 端点的模型通过
`github-copilot@default/<model-id>` 路由（与不带端点后缀的
`github-copilot/<model-id>` 等价）；未声明 `/responses` 的目录条目保留旧路由
`github-copilot@chat-completions/<model-id>`。
升级后，请把已保存的 `github-copilot@responses/<model-id>` 改成
`github-copilot/<model-id>`。对原先使用 Chat Completions 的模型，请重新选择发现的
路由，或在斜杠前加上 `@chat-completions`。
请求使用真实身份 `Copilot-Integration-Id: holon`，不冒充编辑器。
可接受的身份、模型、策略与用量限制由 GitHub 决定。删除本地凭据不会撤销
GitHub 上的授权；停止访问时，还应在 GitHub 设置中撤销 App 授权。

### 场景三：在对话中自检实际模型与为特定 Agent 单独设模

在多 Agent 协作体系中，你通常希望主 Agent 使用通用高性价比模型，而让专门的代码审查员使用高级推理模型。

#### 1. 为特定 Agent 覆盖独立模型

使用 `holon agent model set` 指定单个 Agent 的专属路由：

```bash
# 为专门的代码审查 Agent 指定更强的大模型
holon agent model set "anthropic@default/claude-sonnet-4-6" reviewer

# 可选：随路由一起设置或清除推理强度（reasoning effort）与服务等级（service tier）
holon agent model set "openai@default/gpt-5.4" reviewer --reasoning-effort high --service-tier fast
holon agent model set "openai@default/gpt-6.2-sol" reviewer --service-tier ultrafast
holon agent model set "openai@default/gpt-5.4" reviewer --reasoning-effort clear --service-tier clear

# 查看该 Agent 当前的独立模型配置
holon agent model get reviewer

# 若需清除独立覆盖，恢复继承全局默认配置
holon agent model clear reviewer
```

省略 `--reasoning-effort` 或 `--service-tier` 时会保留该 Agent 当前的对应覆盖配置，传入 `clear` 则清除。

#### 2. 在对话中随时核验实际运行模型

切勿单凭模型在自然语言中的自我介绍（例如“我是由某某公司训练的模型”）来判断当前调用的底细。你可以直接在 Web GUI 或对话中向 Agent 提问：

> “汇报你当前运行环境的实际模型（Active Model）与服务商。”

Agent 会从 Holon 运行时的执行快照中读取权威元数据：
- **`model.requested_model`**：你期望配置的偏好模型。
- **`model.active_model`**：本次请求实际发生物理调用的真实模型。
- **`model.fallback_active`**：是否因主模型故障而触发了备选灾备路由。

### 场景四：配置多云灾备与自动故障回退（Failover）

网络抖动、服务商限流（HTTP 429）或突发服务中断是日常调用大模型最常见的痛点。Holon 提供自动灾备链机制：

1. **启用自动灾备策略（推荐）**：
   ```bash
   holon config set model.fallbacks '["auto"]'
   ```
   在此模式下，当主模型调用失败时，运行时会自动从你已完成认证的其他服务商中挑选最佳备选路由接力，无需中断长任务。
2. **显式配置有序回退链**：
   如果你希望精确控制灾备顺序（例如先尝试云端 DeepSeek，失败后自动切换到 Claude，最后兜底到本地 Ollama）：
   ```bash
   holon config set model.fallbacks '["anthropic@default/claude-sonnet-4-6", "ollama@default/qwen2.5-coder:latest"]'
   ```
3. **强制单一模型（禁用回退）**：
   在执行可复现的基准评测（Benchmarks）或严格限定数据不出境的环境中，可全局禁用自动回退：
   ```bash
   holon config set runtime.disable_provider_fallback true
   ```
   若要恢复灾备能力，重新设为 `false` 即可。

### 场景五：单独配置视觉与生图模型（可选）

文本对话模型并不等同于图像能力。你可以为图像观察与图像生成单独指定专职模型，避免普通对话模型缺乏视觉能力报错：

```bash
# 设置默认图像观察/分析模型
holon config set vision.default "anthropic@default/claude-sonnet-4-6"

# 设置默认图像生成模型
holon config set image_generation.default "openai@default/dall-e-3"
```

这些设置完全独立，不会影响日常文本对话模型的路由。关于图像工具的具体用法，请参阅[检查与分析图像](./inspect-image.md)与[生成图像](./generate-image.md)。

### 场景六：接入自托管 OpenAI 兼容网关（Magpie、LiteLLM 等）

暴露 OpenAI 兼容 `GET /v1/models` 接口的自托管网关可以参与模型自动发现，不必为每个模型手写 `models.catalog` 条目。

1. **注册网关**，只需一个 base URL 和凭据来源：
   ```bash
   holon config providers set my-gateway \
     --transport openai_chat_completions \
     --base-url http://gateway.internal:8080 \
     --credential-source env \
     --credential-kind api_key \
     --credential-env MY_GATEWAY_API_KEY
   ```
   `--base-url` 可以是网关根地址（`http://gateway.internal:8080`），也可以是显式 `/v1` 地址（`http://gateway.internal:8080/v1`），两者都会解析为 `GET /v1/models`；带路径前缀的地址（如 `http://gateway.internal:8080/gw`）解析为 `GET /gw/v1/models`。对开放匿名发现的网关，可以不配置凭据。

   > 自定义（非内置）`openai_chat_completions` provider 的发现地址由 `base_url` 推导：`{base_url}` 或 `{base_url}/v1` 解析为 `{base_url}/v1/models`，路径前缀 `{base_url}/<prefix>` 解析为 `{base_url}/<prefix>/v1/models`。只在根路径提供 `GET /models`（没有 `/v1`）的网关不会被自动发现，请改用 `models.catalog` 登记模型。内置 provider（Copilot、Kilo Code、LiteLLM、vLLM、Ollama 等）都有显式发现定义，不受影响。
2. **触发发现**（发现从不在启动或 Turn 期间探测，因此网关故障不会阻塞 Agent）：
   ```bash
   holon config models refresh --provider my-gateway
   ```
   `providers set` 的输出会报告当前发现缓存状态（`missing`、`stale`、`fresh`、`unauthenticated`、`unsupported`）以及作为 `next_step` 的刷新命令。
3. **核验发现的模型**：
   ```bash
   holon config models list
   ```
   发现的模型会自动出现；用常规路由格式选择：
   ```bash
   holon config set model.default "my-gateway@default/<model-id>"
   ```

发现缓存按推导出的 `/v1/models` 地址隔离。修改 provider 的 `base_url` 会立即让缓存列表失效（即使在 24 小时 TTL 内），下一次刷新即反映新网关。发现失败（404、超时、缺少密钥）会体现在 `models refresh` 结果和 provider 的发现状态中，不会导致启动或 Agent Turn 失败。

`models.catalog` 仍是可选的覆盖手段，用于发现无法提供模型集合的场景，例如设置上下文窗口或输出上限，或非 chat 传输（`anthropic_messages`、`openai_responses` 的模型目录不会自动发现）：
```bash
holon config set models.catalog '{"my-gateway@default/my-model":{"context_window_tokens":131072}}'
```

> 自动发现目前只覆盖 OpenAI 兼容的 `GET /v1/models` 形态，响应中的厂商扩展字段尚未解析；需要显式预算或能力时请使用 `models.catalog` 覆盖。

## 常见排障与诊断

- **本地 Ollama 连接失败（Connection Refused）**：
  检查本地 Ollama 服务是否在后台运行：执行 `curl http://localhost:11434/api/tags` 测试。如果是在 Docker 容器内运行 Holon，请确保使用 `http://host.docker.internal:11434` 作为基地址。
- **环境变量未生效**：
  确认环境变量是否在 Holon 守护进程启动前就已经注入。修改环境变量后，建议运行 `holon config doctor` 验证健康度。
- **单次请求命中非预期服务商**：
  检查输出中是否存在 `fallback_active: true`。这通常说明主模型遭遇了服务商临时限流或网络超时，运行时自动触发了灾备回退。若不希望回退，请配置 `runtime.disable_provider_fallback true`。

---

## 另见

- [模型、提供商与路由设计概念](/zh-CN/concepts/models-and-providers.md)
- [在离线与自托管环境中运行私有 Agent](./run-offline-private-agent.md)
- [本地大模型能跑通多轮编码 Agent 吗？](../blog/local-model-agent-benchmark.md)
- [使用 Web GUI 界面](./use-web-gui.md)
- [配置决策子系统](./configure-decision-subsystem.md)
- [运行无须值守的长周期任务与恢复](./run-long-lived-task.md)
