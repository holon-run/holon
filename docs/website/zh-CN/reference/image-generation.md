---
title: 图像生成
summary: 图像生成工具的参数、支持的模型、输出尺寸和格式，以及错误行为。
order: 46
---

# 图像生成

`GenerateImage` 让 Holon Agent 用配置的图像生成模型从文本提示词创建图片。运行时把生成的
图片保存到 `agent_home/media/generated`，并返回经确认的绝对路径。

## GenerateImage 做什么

Agent 调用 GenerateImage 时，运行时会：

1. **校验提示词和参数**：确保提示词非空，且所有可选参数使用受支持的值。
2. **路由到图像生成模型**：使用配置的 `image_generation.default` 提供商/模型，或自动
   发现已配置轮次模型中第一个支持 `image_generation` 的模型。
3. **保存图片**：把生成的图片字节写入 `agent_home/media/generated`，文件名唯一。
4. **记录持久元数据**：计算 SHA-256，检测尺寸和 MIME 类型，返回绝对路径。

## 参数

> **工具契约参考：** 机器可读的完整工具 JSON Schema、输入约束与返回字段详表，请查阅 [模型工具清单](/zh-CN/reference/model-tool-schema-inventory.md) 与 [工具规格](/zh-CN/spec/tools.md)。

| 参数 | 必填 | 说明 |
|-----------|----------|-------------|
| `prompt` | 是 | 详细的图像生成提示词 |
| `size` | 否 | `1024x1024`、`1536x1024` 或 `1024x1536` 之一 |
| `background` | 否 | `auto`、`transparent` 或 `opaque` 之一 |
| `output_format` | 否 | `png`、`jpeg` 或 `webp` 之一 |
| `name` | 否 | 保存图片的文件名主干 |

## 支持的尺寸

| 尺寸 | 比例 | 典型用途 |
|------|-------|-------------|
| `1024x1024` | 1:1 | 方形图片、图标、社交媒体贴图 |
| `1536x1024` | 3:2 | 横向、横幅、主图 |
| `1024x1536` | 2:3 | 纵向、海报、手机屏幕 |

## GenerateImage 返回什么

每张生成的图片都带持久元数据：

| 字段 | 说明 |
|-------|-------------|
| `id` | 稳定的引用 ID（例如 `img_abc123`） |
| `uri` | 用于 markdown 和 Agent 消息的绝对路径 |
| `mime` | 媒体类型（例如 `image/png`） |
| `byte_count` | 文件大小（字节） |
| `sha256` | 内容哈希 |
| `size` | 可检测时的图片尺寸（宽 × 高） |
| `created_at` | 生成时间戳 |

结果还包含提供商/模型来源和原始提示词。

## 图片路由

### 显式配置

设置专用的图像生成模型：

```bash
holon config set image_generation.default "openai/gpt-image-2"
```

配置了 `image_generation.default` 时，GenerateImage 对所有图像生成请求都用该模型。使用
包含端点的模型路由引用可以让路由无歧义：

```bash
holon config set image_generation.default "volcengine@image-openai/doubao-seedream-5.0-lite"
```

### 自动发现

未设置 `image_generation.default` 时，运行时选择第一个声明 `image_generation` 能力的已
配置轮次模型。这样，只要当前对话模型支持该能力，Agent 就能在没有专门图像生成配置的情况下
生成图片。

### 回退提供商

当模型本身不支持图像生成时，`GenerateImage` 回退到第一个已配置且启用图像生成的模型。这对
Agent 是透明的。

## 支持的提供商

图像生成通过声明 `image_generation` 能力的模型所属的提供商支持：

| 提供商 | 模型 | 说明 |
|----------|-------|-------|
| OpenAI | gpt-image-2 | 原生图像生成 |
| Volcengine | doubao-seedream-5.0-lite | 经 Volcengine Ark plan 端点 |
| xAI | grok-imagine-image-2.0 | 经 xAI OpenAI 兼容 Images API |

> 当前支持图像生成的模型列表见[模型参考](/zh-CN/reference/models.md)。

## Volcengine Seedream 设置

要用 Volcengine 的 Seedream 模型做图像生成，先配置一个专用的 plan 端点：

```bash
holon config set providers.volcengine.endpoints.image-openai.transport openai_chat_completions
holon config set providers.volcengine.endpoints.image-openai.base_url "https://ark.cn-beijing.volces.com/api/plan/v3"
holon config set providers.volcengine.plans.image-openai.endpoint image-openai
```

然后设置图像生成默认值：

```bash
holon config set image_generation.default "volcengine@image-openai/doubao-seedream-5.0-lite"
```

## xAI Grok Imagine 设置

xAI Grok Imagine 生图模型通过 xAI 的兼容 OpenAI Images API 暴露。因为它们是仅用于生图的模型，不参与对话轮次候选，所以需要显式配置默认生图路由：

```bash
holon config set image_generation.default "xai@grok-imagine-image-2.0"
```

受支持的模型包括 `grok-imagine-image`、`grok-imagine-image-2.0` 与 `grok-imagine-image-quality`。`xai` 提供商自动解析 `XAI_API_KEY` 或 Holon 托管的 xAI OAuth 授权配置。

xAI 接口不接受 OpenAI 风格的 `size`、`background` 或 `output_format` 请求字段。Holon 运行时将 `size` 映射为 `1k` 级别最接近的宽高比（`1024x1024` 映射为 `1:1`，`1536x1024` 映射为 `3:2`，`1024x1536` 映射为 `2:3`）。若请求传入了不支持的 `background` 或 `output_format`，运行时会直接报错而不是静默忽略。保存文件时，运行时依据 xAI 返回的实际媒体类型匹配文件扩展名。

## 输出管理

生成的图片以带时间戳的文件名保存到 `agent_home/media/generated/`。提供了 `name` 时，文件名
用该主干；否则默认为 `generated_<timestamp>`。

返回的绝对路径可用于 Agent 的 markdown 输出，在对话中直接渲染。图片可通过 Web GUI
文件浏览器和 workspace 文件 API 访问。

## Agent 何时使用 GenerateImage

任务涉及视觉创作时，Agent 会调用 GenerateImage：

- **数据可视化**：根据数据描述生成图表、示意图或信息图。
- **UI 稿**：创建界面概念的视觉呈现。
- **Logo 和图标设计**：生成品牌素材或图标。
- **插画**：为文档或演示创建视觉辅助素材。

该工具属于 `LocalEnvironment` 能力族，默认对每个 Agent 可用。

## CLI 验证

GenerateImage 是面向模型的工具，不能直接从 CLI 调用。Agent 通过正常的工具调用机制使用
它。图像生成模型路由可以用以下命令查看：

```bash
holon config get image_generation.default
```

## 另见

- [模型工具 schema 清册](/zh-CN/reference/model-tool-schema-inventory.md)：工具注册与稳定性
- [模型参考](/zh-CN/reference/models.md)：支持的提供商和图像生成可用性
- [配置参考](/zh-CN/reference/configuration.md)：`image_generation.default` 和提供商设置
- [Web GUI 指南](/zh-CN/guides/use-web-gui.md)：浏览器中的图像生成设置
