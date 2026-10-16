---
title: 文档工作流
summary: 如何编辑和构建由 mdorigin 驱动的 Holon 网站。
order: 20
---

# 文档工作流

Holon 网站源码位于 `docs/website/`，构建时会在 `.tools/dist/content/` 中准备隔离的 mdorigin 内容根。源页面是 Markdown 文件，可以渲染为 HTML，也可以按 Markdown 获取。

## 编辑内容

在 `docs/website/` 下新增或更新 Markdown 文件。用各目录的 `README.md` 作为分区落地页：

```text
docs/website/
  README.md
  concepts/
    README.md
    runtime-model.md
  guides/
    README.md
```

面向公众的网站文案保持简洁。详细的运行时契约和设计记录仍放在仓库的 `docs/` 目录下。

## 检查契约引用

文档 CI 会检查以下位置中仓库本地的 Markdown 链接和标题锚点：

- `README.md`、`docs/architecture-overview.md` 和 `docs/runtime-spec.md`
- `docs/website/spec/` 和 `docs/website/reference/`
- `docs/rfcs/` 中的真实 Markdown 链接

专题规范还可以在 `Last verified` 引用块或 `Implementation references` 一节中声明实现路径。把仓库路径写成代码片段，例如：

```markdown
> **Last verified:** against `src/runtime/scheduler.rs` and
> `src/runtime/waiting.rs`.
```

其他位置需要强制校验某个源码路径时，用真实的 Markdown 链接：

```markdown
[`src/http/mod.rs`](../../../src/http/mod.rs)
```

围栏示例、占位符和普通代码片段不会被当作当前的实现契约。如果某条本应被检查的行上是历史或提议的引用，只有在该引用之后紧接一条非空原因说明时，才能把它排除：

```markdown
`src/runtime/proposed.rs` <!-- contract-ref-ignore: proposed file from accepted RFC -->
```

在仓库根目录运行检查：

```bash
npm --prefix docs/website/.tools run prepare:content
npm --prefix docs/website/.tools run check:content
python3 docs/website/.tools/test-check-contract-refs.py
python3 docs/website/.tools/check-contract-refs.py
```

失败时用 `file:line:target` 格式给出诊断，方便编辑器和 CI 日志直接定位声明位置。
链接与导航统一使用准备好的内容根检查，其中包含生成路由。

## 本地预览

使用 Node.js 22 和 Python 3.11 或更高版本。在仓库根目录运行：

```bash
npm ci --prefix docs/website/.tools
npm --prefix docs/website/.tools run dev
```

`dev` 在启动时生成内容快照，预览服务读取该快照，而不是源码树。修改任何网站正文
（包括普通 Markdown）、配置、样式、模板或展示元数据后，都需要重启预览。
仅正文改动时，也可另行运行 `npm --prefix docs/website/.tools run prepare:content`
刷新生成文件；配置和样式由 mdorigin 在启动时加载，必须重启才能更新。
源码改动不会自动复制；目前没有监听源码树的 watcher。

## 刷新索引

文档目录页只用一份受管理的索引列出本节文章。正文可保留导读和跨分区链接，
但不要复制目录，也不要把索引翻译成正文里的第二份列表。翻译各文章的
`title` 和 `summary` 后重新生成索引即可。在 `docs/website` 下运行
`npm --prefix .tools run check:directories` 检查生成目录页中的重复链接；运行前先准备内容并构建索引。

目录页可以包含受管理的索引块：

```markdown
<!-- INDEX:START -->
<!-- INDEX:END -->
```

在 `docs/website` 下运行以下命令，在隔离内容根中重新生成：

```bash
npm --prefix .tools run prepare:content
npm --prefix .tools run build:index
npm --prefix .tools run check:directories
```

## 构建可部署产物

在 `docs/website` 下运行：

```bash
npm --prefix .tools run build
```

构建只准备一次内容，后续目录索引、生成链接与目录检查、搜索索引和 Cloudflare
打包都使用同一个内容根。搜索与部署产物仍分别位于 `.tools/dist/search/` 和
`.tools/dist/cloudflare/`。生成的 `.tools/dist/` 已被忽略，不应提交，也不要把生成目录复制回源码页面。

## 模板目录

模板目录读取仓库本地的 `agent_templates/*/template.toml` 和 `skills.toml`。
`.tools/build-template-catalog.py` 复制网站内容（不包含 `.tools`），并在
`.tools/dist/content/` 中生成首页精选、完整目录页和 JSON 资源。该脚本可以从任意当前目录运行。
构建不会通过网络拉取模板，也没有目录 API：模板数据与文档来自同一次 Git 提交 SHA。

分类、精选顺序和可选的中文展示名称及摘要维护在 `template-display.json`，
角色契约仍以模板文件为准。缺少中文展示文案时回退到模板的英文元数据；
没有展示配置的模板仍会出现在完整目录中，并使用兜底分类。
删除精选模板后，构建会报错，直到在 `template-display.json` 中移除或替换对应的失效精选项。

CI 会验证模板与生成器测试，保留契约引用和生成副本检查，并使用实际内容根检查全部复制的源码、生成页面链接及导航。
PR 只构建和上传产物，不部署。

## 生成页面与多语言

有些页面由生成器产出，而不是人工撰写。目前只有 `reference/models.md`，生成命令为：

```bash
cargo run --bin holon-docgen -- models > docs/website/reference/models.md
```

不要人工翻译生成页面：下一次重新生成会覆盖翻译。应把该页面登记到
`.tools/generated-pages.json`，然后同步各语言副本：

```bash
npm --prefix .tools run sync:generated
```

脚本会把生成的英文页面复制到各语言路径，替换清单中列出的 front matter 字段为本地化文案，
并加上一条正文由生成器产出的说明。复制结果与原页面一起提交。文档 CI 会以 `--check`
运行同一脚本，一旦生成页面与其副本不一致就失败，因此重新生成后只需多跑一条命令，
不需要重新翻译。

## 语言自动检测

站点包含两个语言：`en`（默认）与 `zh-CN`。`mdorigin.config.json` 启用了
`localeDetection`，因此访问 `/` 时会按访问者偏好语言重定向一次到对应语言根路径。
深层链接不会被重定向，通过页头语言切换器做出的选择优先，爬虫始终停留在默认语言根路径。

## 刷新生成的契约快照

OpenAPI、HTTP 路由、CLI、运行时状态枚举和模型工具 schema 快照由主 CI 单独检查。发布契约变更前运行 `make snapshots-check`。如果变更是有意为之，运行 `make snapshots-refresh`，审阅生成的 diff，然后重新运行 `make snapshots-check`。

## 发布说明

`siteUrl` 配置为 `https://holon.run`，因此发布会为生产域名暴露规范的 sitemap 和 feed URL。
