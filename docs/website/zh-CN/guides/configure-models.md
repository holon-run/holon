---
title: 配置模型、凭据与回退
summary: 配置服务商凭据、指定全局或 Agent 级路由，并验证单次交互中使用的实际路由。
order: 130
---

# 配置模型、凭据与回退

在为 Agent 指定模型之前，先确保拥有可用的提供商连接。关于路由标识定义与回退机制，请参阅[模型、提供商与路由](/zh-CN/concepts/models-and-providers.md)。

## 前置条件

- 具备对目标 Holon 运行时的操作权限。
- 拥有模型提供商账号或本地模型推理服务。
- 具备修改共享配置的权限。全局默认配置与凭据会影响多个 Agent。

请在操作者终端中运行以下命令。切勿将敏感凭据直接粘贴至 Agent 对话、截图、执行日志或 Issue 报告中。

## 配置步骤

1. 检查已注册的提供商与配置健康状态：

   ```bash
   holon config providers list
   holon config doctor
   ```

   若需交互式配置（包括支持 OAuth 或设备登录的服务商）：

   ```bash
   holon onboard
   ```

   根据向导完成所选服务商的配置。若使用 API Key，建议通过标准输入保存，再将配置 Profile 关联至提供商。以下以内置的 `deepseek` 提供商为例：

   ```bash
   holon config credentials set --kind api_key --stdin deepseek
   ```

   输入 Key 并按 Ctrl+D 结束输入，随后执行关联命令：

   ```bash
   holon config set providers.deepseek.auth.source credential_profile
   holon config set providers.deepseek.auth.profile deepseek
   holon config set providers.deepseek.auth.kind api_key
   ```

   Profile 名称属于自定义标识，不会自动映射到同名提供商。另外，也可以在启动守护进程的环境中直接注入对应的环境变量；而在独立的客户端终端中 export 环境变量并不会影响已在后台运行的守护进程。切勿在共享命令历史中使用 `--material` 传递明文密钥。

2. 认证完成后列出可用模型：

   ```bash
   holon config models list
   ```

   从输出中选择可用的交互路由。标准路由格式为 `provider@endpoint/model`，例如 `deepseek@default/deepseek-v4-pro`。实际可用性仍取决于你的账号权限、网络端点与配置情况。

3. 将选定的路由设为全局默认：

   ```bash
   holon config set model.default "deepseek@default/deepseek-v4-pro"
   holon config get model.default
   ```

   若要为某个已有 Agent 单独覆盖配置：

   ```bash
   holon agent model set "anthropic@default/claude-sonnet-4-6" reviewer
   holon agent model get reviewer
   ```

   请使用当前环境中真实可用的路由；上述两个示例需要不同的提供商连接。执行 `holon agent model clear reviewer` 可清除单独覆盖，让该 Agent 恢复使用全局默认配置。

4. 配置回退策略：

   ```bash
   # 自动回退：每个已认证提供商自动挑选一个首选路由
   holon config set model.fallbacks '["auto"]'

   # 或显式指定一组已完成认证的有序回退路由
   holon config set model.fallbacks '["anthropic@default/claude-sonnet-4-6"]'
   ```

   仅保留你打算使用的回退策略。Agent 级别的覆盖仅替换主路由，全局默认不会隐式作为回退追加在末尾。未配置或为空时系统使用自动回退。若要强制限定单一路由并完全禁用回退，可执行：

   ```bash
   holon config set runtime.disable_provider_fallback true
   ```

   该设置为全局生效。需要恢复回退时将其重置为 `false`。在启用附加提供商前，请确认提示词可能发送到的第三方范围。

5. 留意命令在 stderr 输出的提示。部分持久化改动可能需要重启守护进程才能生效。若有提示，等待当前活跃工作平稳结束后，按标准流程重启守护进程。随后发起单次测试：

   ```bash
   holon config doctor
   holon run --agent reviewer "用一句话输出问候语。不要使用任何工具，不要修改任何文件。"
   holon agent get reviewer --detail full
   holon transcript --agent reviewer --limit 20
   ```

## 确认配置生效

模型列表与 `doctor` 报告应当显示主路由可用。测试请求会返回一段简短回复。在 Agent 详情中，检查 `model.requested_model`、`model.active_model` 以及 `model.fallback_active`（若存在），并对照对话记录中的提供商元数据。这有助于分清所请求的偏好路由与发生回退后实际使用的路由。切勿单凭模型在自然语言回复中的自我介绍来判断。

## 本地或自定义端点

若使用本地 Ollama 服务，先在 Ollama 中拉取模型，然后选择其可用路由（例如 `ollama@default/<installed-model>`）。对于默认的本机 Ollama 服务，无需配置 API Key。

若配置自定义提供商，请使用专用管理命令：

```bash
holon config providers set my-proxy \
  --transport openai_chat_completions \
  --base-url "https://my-proxy.example.com/v1" \
  --credential-source env \
  --credential-env MY_PROXY_API_KEY \
  --credential-kind api_key
```

替换端点并在守护进程环境中配置凭据。在正式选用前先执行 `holon config models list` 检查。端点、计划与模型元数据字段详见[配置参考](/zh-CN/reference/configuration.md)。

## 单独配置图像工具

挑选支持对应能力且守护进程已具备访问凭据的路由。例如，在配置好对应提供商后：

```bash
holon config set vision.default "anthropic@default/claude-sonnet-4-6"
holon config set image_generation.default "openai@default/gpt-image-2"
holon config get vision.default
holon config get image_generation.default
```

这些设置不会影响日常对话模型。遵循可能出现的重启提示，随后使用无害的本地图像测试[图像观察](inspect-image.md)，或发送简单请求测试[图像生成](generate-image.md)。重点检查工具的实际返回结果与提供商选择，而非仅看存储的配置。图像生成可能会产生第三方服务费用。

若未显式指定专用路由，工具将按内置发现规则运作。能力与传输协议要求详见[视觉模型选择](../reference/view-image.md#vision-model-selection)与[生图路由](../reference/image-generation.md#image-routing)。单纯文本对话能够正常运行，不能证明图像工具已经就绪。

## 故障排查

- **凭据缺失或过期：** 针对 OAuth 重新运行 `holon onboard`，或通过标准输入更新凭据 Profile。检查该提供商配置的凭据来源。
- **路由未知或不可用：** 从模型列表中复制完整路由标识，并检查端点与账号权限。元数据声明并不会自动为账号开通模型权限。
- **命中了非预期的提供商：** 检查 Agent 覆盖设置、回退策略以及实际的执行元数据，评估是否需要全局禁用回退。
- **存储的配置与运行时表现不一致：** 查看 stderr 中的生效途径说明或重启提示。避免重复写入同一个配置键。
- **自定义编目修改报错：** `models.catalog` 属于对象值。添加单条覆盖时务必保留不相关的现有项，避免误用单项字典覆盖整个映射表。

关于仅用于非权威建议的决策模型配置，请参阅[配置咨询决策](/zh-CN/guides/configure-decision-subsystem.md)。
