---
title: 图像生成指南
summary: 从文本提示词生成图片的 Agent 工具：模型选择、尺寸和格式选项、输出管理。
order: 47
---

# 图像生成指南

`GenerateImage` 让 Holon Agent 用配置的图像生成模型从文本提示词创建图片。运行时把生成的
图片保存到 `agent_home/media/generated`，并返回持久化的 workspace URI。

## GenerateImage 做什么

Agent 调用 GenerateImage 时，运行时会：

1. **校验提示词和参数**：确保提示词非空，且所有可选参数使用受支持的值。
2. **路由到图像生成模型**：使用配置的 `image_generation.default` 提供商/模型，或自动
   发现已配置轮次模型中第一个支持 `image_generation` 的模型。
3. **保存图片**：把生成的图片字节写入 `agent_home/media/generated`，文件名唯一。
4. **记录持久元数据**：计算 SHA-256，检测尺寸和 MIME 类型，返回 `workspace://` URI。

## 参数

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
| `uri` | 用于 markdown 和 Agent 消息的 `workspace://` URI |
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

## 输出管理

生成的图片以带时间戳的文件名保存到 `agent_home/media/generated/`。提供了 `name` 时，文件名
用该主干；否则默认为 `generated_<timestamp>`。

返回的 `workspace://` URI 可用于 Agent 的 markdown 输出，在对话中直接渲染。图片可通过
Web GUI 文件浏览器和 workspace 文件 API 访问。

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
- [Web GUI 指南](/zh-CN/guides/web-gui.md)：浏览器中的图像生成设置
