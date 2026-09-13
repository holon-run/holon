---
title: 文档工作流
summary: 如何编辑和构建由 mdorigin 驱动的 Holon 网站。
order: 20
---

# 文档工作流

Holon 网站是 `docs/website/` 下的一个 mdorigin 内容根。源页面是 Markdown 文件，可以渲染为 HTML，也可以按 Markdown 获取。

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
python3 docs/website/.tools/check-links.py
python3 docs/website/.tools/test-check-contract-refs.py
python3 docs/website/.tools/check-contract-refs.py
```

失败时用 `file:line:target` 格式给出诊断，方便编辑器和 CI 日志直接定位声明位置。

## 本地预览

```bash
cd docs/website
mdorigin dev --root .
```

## 刷新索引

目录页可以包含受管理的索引块：

```markdown
<!-- INDEX:START -->
<!-- INDEX:END -->
```

用下面的命令重新生成：

```bash
mdorigin build index --root .
```

## 构建可部署产物

```bash
mdorigin build search --root . --out dist/search
mdorigin build cloudflare --root . --search dist/search
```

生成的 `dist/` 目录已被忽略，不应提交。

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

## 刷新生成的契约快照

OpenAPI、HTTP 路由、CLI、运行时状态枚举和模型工具 schema 快照由主 CI 单独检查。发布契约变更前运行 `make snapshots-check`。如果变更是有意为之，运行 `make snapshots-refresh`，审阅生成的 diff，然后重新运行 `make snapshots-check`。

## 发布说明

`siteUrl` 配置为 `https://holon.run`，因此发布会为生产域名暴露规范的 sitemap 和 feed URL。
