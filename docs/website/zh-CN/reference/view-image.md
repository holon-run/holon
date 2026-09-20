---
title: 图像观察
summary: 图像观察工具的输入、视觉模型选择、响应元数据和兼容性限制。
order: 44
---

# ViewImage 指南

`ViewImage` 让 Holon Agent 通过视觉模型检查本地图片文件。Agent 提供图片路径和描述要检查
内容的提示词，运行时返回结构化的视觉观察结果。

## ViewImage 做什么

Agent 调用 ViewImage 时，运行时会：

1. **校验图片**：读取文件，检查体积上限（最大 20 MB、5000 万像素），计算 SHA-256，
   检测 MIME 类型和尺寸。
2. **选择视觉模型**：使用配置的 `vision.default` 提供商/模型，或自动发现一个支持图片输入
   且已认证的提供商。
3. **生成观察结果**：把图片和提示词发给视觉模型，返回结构化观察结果。
4. **缓存结果**：之后用相同图片和提示词调用时复用缓存的观察结果。

## 参数

| 参数 | 必填 | 说明 |
|-----------|----------|-------------|
| `path` | 是 | workspace 相对路径或绝对路径的图片路径 |
| `prompt` | 是 | 要在图片中检查或描述的内容 |

支持的图片格式：PNG、JPEG、GIF、WebP，以及所选视觉模型支持的其他格式。

## ViewImage 返回什么

结果分两部分：

### 视觉引用（持久元数据）

无论视觉模型是否可用，每次调用都会记录：

| 字段 | 说明 |
|-------|-------------|
| `id` | 稳定的引用 ID |
| `mime` | 媒体类型（例如 `image/png`） |
| `byte_count` | 文件大小（字节） |
| `sha256` | 内容哈希 |
| `path` | 解析后的文件路径 |
| `size` | 可检测时的图片尺寸（宽 × 高） |

### 视觉观察（生成的）

视觉模型可用时，观察结果包含：

| 字段 | 说明 |
|-------|-------------|
| `generated_by` | 提供商、模型和生成模式 |
| `prompt` | 产生该观察结果的提示词 |
| `summary` | 观察结果的可读摘要 |
| `ocr` | 提取的文本（适用时） |
| `elements` | 识别出的视觉元素 |
| `relations` | 元素之间的空间或逻辑关系 |
| `issues` | 检测到的问题或异常 |
| `uncertainties` | 模型不确定的地方 |

## 视觉模型选择

### 显式配置

设置专用的视觉模型：

```bash
holon config set vision.default "anthropic/claude-sonnet-4-6"
```

配置了 `vision.default` 时，ViewImage 对所有图片观察都用该模型。这是生产环境的推荐做法。

### 自动发现

未设置 `vision.default` 时，ViewImage 会扫描已配置的提供商，自动发现可用的视觉模型，
条件包括：

- 凭据有效
- 声明支持 `image_input`
- 使用支持生成图片观察结果的 transport（OpenAI 兼容 API 或 Anthropic Messages）

选择结果会随工具响应一起返回，你能看到选了哪个模型。

### 没有可用视觉模型时

如果没有已配置的模型支持图片输入，ViewImage 返回 `vision_adapter_unavailable` 错误，
并附带评估过的候选列表。持久视觉引用元数据仍会记录。

## 观察结果缓存

ViewImage 用「图片哈希 + 提示词」的组合键缓存观察结果。如果 Agent 用相同图片和提示词再次
调用 ViewImage，运行时直接返回缓存结果，不再调用模型：

```
ViewImage reused cached visual observation
```

在 Agent 反复查看同一张图片的多轮会话中，这能省下延迟和成本。

## Agent 何时使用 ViewImage

任务涉及视觉检查时，Agent 会调用 ViewImage：

- **从截图做代码审查**：检查 UI 稿、错误画面或示意图。
- **文档分析**：从扫描件、收据或白板照片中提取文本或结构。
- **排查问题**：诊断生成输出或测试失败中的视觉异常。
- **数据提取**：从以图片呈现的图表、表格或表单中提取结构化数据。

Agent 根据任务上下文判断 ViewImage 是否相关。该工具属于 `LocalEnvironment` 能力族，
默认对每个 Agent 可用。

## CLI 验证

ViewImage 是面向模型的工具，不能直接从 CLI 调用。Agent 通过正常的工具调用机制使用它。

## 另见

- [模型工具 schema 清册](/zh-CN/reference/model-tool-schema-inventory.md)：工具注册与稳定性
- [模型参考](/zh-CN/reference/models.md)：支持的提供商和视觉模型可用性
- [配置参考](/zh-CN/reference/configuration.md)：`vision.default` 和提供商设置
