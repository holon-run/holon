# Lite paper 编辑记录

本文件不进入 PDF。中文正文以 `zh-CN.md` 为准。


### 本轮内容调整

1. 将 webhook 与定时触发提升为核心定位，说明 Agent 如何从系统事件和时间安排接到工作。
2. 明确展示模板、按 Agent 管理技能、多工作区、Web / TUI、多模型和二进制内嵌 Web GUI。
3. 将原来的后台执行例子并入持续审阅，避免用整页只解释关闭界面。
4. 新增跨工作区交付例子，保留子任务监督和 worktree 的作用。
5. 将团队共享 Agent 提升为独立主案例，使用现有实践材料，避免三个案例都停留在假设流程。
6. 以 `zh-CN.md` 为唯一正文；内容确认后再发布 PDF，之前的六页 PDF 为排版参考。

### 差异化表述

重点解释工作如何发起：从人逐次发起的交互，扩展到外部事件、时间安排与人工输入共同驱动。Holon 将这些入口与长期角色、工作状态和执行环境组合在一起。

对“主动性”的表述应包含触发来源、已有职责和实际行动。不能仅用“后台常驻”代表主动工作，也不能仅用“支持 webhook”代替用户价值。本文不作“Codex 等工具不能定时或接入 CI”的排他性声明；如需加入具体竞品功能对照，再按产品形态与版本逐项核实。

### 轻量部署：值得展示，待补可比较的数据

“体积小、占用内存小”与常驻运行、个人部署和团队自托管直接相关，适合在开篇做数字摘要，在部署章节给出测量口径。正文已先写入可确认的一体化部署能力；体积与内存数字取得依据后再补入。

| 候选指标 | 发布前需记录的条件 | 当前依据 |
| --- | --- | --- |
| 发布包下载大小 | 版本、操作系统、架构、压缩格式 | 待测量目标发布资产 |
| 解压后二进制大小 | 版本、平台、构建配置、是否内嵌 GUI | 本地 `target/release/holon` 为 macOS arm64，69,808,960 字节，约 66.58 MiB；这是本地构建快照，不代表所有发布包 |
| 空闲常驻内存 | OS、架构、Agent 数量、历史规模、预热时间与 RSS 采样窗口 | 尚未测量 |
| 典型工作负载内存 | 工作内容、并发量、采样时长、平均及峰值 RSS | 尚未测量 |

测量时应区分 Holon 进程、浏览器、子命令和本地模型的占用。已有 `memory_lifecycle` benchmark 面向局部生命周期回归，不等同于 daemon 常驻内存测量。没有可复现条件前，不写“极低内存”“只有几 MB”或相对其他产品的倍数优势。

### 团队实践的使用边界

本稿引用团队文章的具体工作过程，没有把 Token 用量当作效率证据，也没有把报告覆盖数改写为解决率。若后续需要数据卡片，可从原文选择调查或验收覆盖指标，同时保留时间窗口、去重方式与“覆盖不等于解决”的说明。

### 内容依据

本轮读取的本地资料如下；产品功能依据本地文档核对，团队案例为对既有文章的归纳。

- [项目定位与安装](../../README.md)
- [Holon 是什么](../website/zh-CN/blog/what-is-holon.md)
- [Agent Templates](../website/guides/agent-templates.md)
- [Skills：技能库与按 Agent 启用](../website/guides/skills.md)
- [多工作区与 worktree](../website/guides/workspaces.md)
- [持续工作流程](../website/guides/durable-agent-workflow.md)
- [Webhook 与定时触发](../website/concepts/external-triggers.md)
- [定时器 CLI：延时与周期任务](../website/reference/cli.md)
- [WorkItem](../website/guides/work-items.md)
- [子 Agent 与监督](../website/guides/multi-agent.md)
- [Web GUI](../website/guides/web-gui.md)
- [远程访问](../website/guides/remote-access.md)
- [团队共享 Agent 实践](../website/zh-CN/blog/agents-in-a-small-team.md)
- [执行与安全边界](../website/concepts/security-and-execution-boundaries.md)
- [内存 benchmark 的测量范围](../../benchmarks/README.md)

### 本次目录整理与构建状态

- 中文正文迁入 `zh-CN.md`，本文件保存讨论记录与依据；英文尚未开始。
- `tools/build.py` 直接读取正文，`tools/layout.py` 提供共享排版，生成物写入已忽略的 `build/lite-paper/`。
- 原六页脚本保存为 `tools/archive/layout-v0.3.py`，仅用于查阅矢量图形和布局实现，不是构建入口，不再维护其中的文案。旧 PDF 和其他中间文件保留在本地 `build/lite-paper/history/`。
- 当前完整 Markdown 比原六页 PDF 的人工摘要长，基础生成器允许分页；后续把六页精排图示迁入共享布局时，内容仍应来自 Markdown。
- 可再分发的统一字体尚待选定；当前允许显式提供字体，并保留 macOS 本地字体回退，不将系统字体提交仓库。
- 未向网站发布 PDF，未修改首页；先审阅中文内容与版式，再增加正式下载资产。

### 本次验证

- 中文正文与迁移前 v0.3 正文一致，仅分离编辑备注、添加元数据、修正相对链接并标准化 Markdown 换行标记。
- 使用 ReportLab 4.4.9 在 macOS arm64 成功生成 9 页基础排版和对应预览；来源、模板与字体摘要已记录。
- 检查了本地链接、PDF 功能文本、编辑备注排除，以及英文稿缺失、误写网站目录、未闭合代码块和非法表格的失败行为。
- 已浏览全部预览，未发现文字裁切；文字图示的等宽对齐和章节分页留白仍待下一轮精排，这份构建产物不是网站发布版。
- Linux 与统一可再分发字体尚未验证。Rust 运行时代码没有改动，因此没有运行 Cargo 构建或测试。
