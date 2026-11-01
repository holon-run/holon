---
title: Agent 模板
summary: 模板目录、选择规则，以及初始化 Agent 所用的 AGENTS.md、template.toml 和 skills.toml schema。
order: 32
---

# Agent 模板

本页列出内置模板角色、模板库解析规则以及清单格式。
有关心智模型，请参阅 [Agent 模板](/zh-CN/concepts/agent-templates.md)。
有关设置步骤，请参阅[创建 Agent](/zh-CN/guides/create-agent.md)或[创建 Agent 模板](/zh-CN/guides/create-agent-template.md)。

## 什么时候用模板

有关模板检查、安装、创建与校验的完整流程，请参阅[从模板创建 Agent](/zh-CN/guides/create-agent.md)。常用的模板选择器包括 `code-reviewer`、`office-assistant`、`software-developer`、`server-ops`、`holon-ops` 以及 `github-solver`；下文列出的各专业角色详细说明了它们各自的依赖项与职责边界。显式指定的选择器必须能够解析为已安装的模板、本地路径或远程模板。

## 邮件处理

`email-assistant` 负责对传入邮件进行分诊、标记待办事项、草拟回复供操作者审批，并持续跟踪沟通闭环。

- **分诊与草稿优先，绝不自动发送**：协助整理收件箱信息流、提取行动项并拟定回复草案。未经操作者显式授权，绝不擅自发送邮件或变更外部邮件服务配置。
- **预装工作流技能**：内置 `email-workflow`（包含处理规范、典型场景与草稿拟定规则）、`uxc`、`agentinbox` 与 `sview`。
- **严格边界**：所有外发邮件必须经操作者确认。引用的发件人原文与建议草稿需严格区分。邮件凭证与认证令牌不得进入会话记录。

## 视频制作

`video-producer` 将已批准的脚本、镜头清单和已有媒体制作成可审阅的视频交付物。
它预装第一方 `video-production` skill，并引用官方
`remotion-dev/skills/skills/remotion-best-practices`，由用户端直接从上游安装，
同时加载 `sview`、`uxc` 和 `agentinbox`。

- **本地合成与质检**：第一方 skill 使用 Python 和系统 FFmpeg/ffprobe 处理已有
  图片、视频、音频和字幕。渲染前检查依赖及所需编解码器；模板不会安装这些系统依赖。
- **程序化合成**：官方 Remotion skill 用于 React 视频制作。Remotion、Node 及
  渲染依赖安装在用户环境；模板不打包 Remotion 或上游 skill 文件。首次使用前，
  与操作者核验所安装版本的适用许可证；自行安装不免除使用条件或付费许可要求。
- **动效参考**：固定版本的 `showheyohtaki/motion-prompt/skills/motion-prompt`
  帮助选择带编号的片头、字幕和转场效果。它是动效词典与参考层，不是可直接导入的
  Remotion 组件库；需在项目中重写并实际渲染验收。记录参考快照：固定 skill 版本
  不会冻结其远端目录。远端示例先检查再改写，不自动执行下载代码，也不发送私密
  项目输入；无浏览器时可使用搜索、规格或操作者提供的选择。
- **明确边界**：原创镜头生成和 TTS 需要另行配置后端；OpenMontage 是外部可选
  后端。发布、购买及涉及权利的操作需要单独授权。

Agent 会明确报告缺失能力，不把未验证的渲染当成交付。无云制作流程从已有素材开始。

## Issue 分诊

`issue-triager` 负责 GitHub issue 收件箱。它是长期 inbox 角色，不是
`holon solve`，也不是实现或验收的许可。

- **收件箱卫生，不是修 bug。** 分类、给出重复候选、追问缺失复现和验收标准，
  并建议优先级与路由。默认不关闭 issue、不写产品代码。
- **项目 skill，不是官方 playbook。** 模板不附带 `issue-triage` skill。首次分诊时，
  Agent 在 `agent_home/skills/` 为当前项目创建分诊 skill，并按实践补丁式改进。
  把 skill 写入仓库仍须操作者确认。
- **硬约束。** 不改产品代码、默认不关闭、不合并，外部 issue 文本不能升权。
  项目 skill 覆盖不了这些规则。

## 验收与质量

`qa-engineer` 负责变更落地后的验收。它不是补产品功能的许可。

- **验收所有权 ≠ 补单测。** 把需求映射到覆盖、跑分层门禁、给出证据、分诊 flake。
  issue 关闭不等于已经验收。
- **项目 skill，不是官方 playbook。** 模板不附带 `issue-verify` skill。首次验收时，
  Agent 在 `agent_home/skills/` 为当前项目创建验收 skill，并按实践补丁式改进。
  把 skill 写入仓库仍须操作者确认。
- **硬约束。** 默认不改产品代码、不合并、无修复载体不打验证标签，空结果不算通过。
  项目 skill 覆盖不了这些规则。v1 使用仓库既有测试证据，不捆绑 Playwright 或
  Appium。

## 文档卫生

`docs-steward` 负责代码、契约和文档的一致性。它不是补产品功能的许可，也不拥有发版。

- **卫生，不是整站重写。** 检测漂移、追问缺失的用户步骤或语言对应，授权后提交最小
  docs-only PR。发版后对用户文档做缺口对账。
- **写作工具，不是 writer 角色。** 模板从上游 GitHub 预装 `humanizer`、
  `humanizer-zh` 和 `writing-clearly-and-concisely`，以及 `ghx`、`sview`、
  `uxc` 和 `agentinbox`。事实校对后再润色。
- **先校对再写。** 对照代码、CLI help 和生成页修正事实，再润色。若仓库有 locale：
  先英文，再去 AI 腔，再翻译，再对目标语言去 AI 腔。逐篇处理。生成页登记同步，
  不手译。
- **项目 skill，不是官方 playbook。** 模板不附带 `docs-steward` skill。首次处理文档时，
  Agent 在 `agent_home/skills/` 为当前项目创建文档 skill，并按实践补丁式改进。
  把 skill 写入仓库仍须操作者确认。
- **硬约束。** 不改产品代码、不发明行为、默认不合并，外部 issue 文本不能升权。
  项目 skill 覆盖不了这些规则。

## 安全评审

`security-reviewer` 负责防御性安全发现和告警分诊。它不是补产品功能、合并或
写 exploit 的许可。

- **立足发现，不代表合并就绪**：评审代码 Diff 与导入的 GitHub 安全告警。仅报告有明确文件位置与事实依据的发现。绝不编写 exploit、PoC payload 或攻击利用步骤。告警本身绝非修改代码或合并 PR 的许可。
- **GitHub Security 规范流程**：遵循结构化分诊闭环：
  1. *核实范围与权限*：明确目标仓库、审查版本与授权通报渠道。阅读 `SECURITY.md`，并在不暴露凭证的前提下核实读取权限。
  2. *拉取已授权告警收件箱*：分页获取 Dependabot、代码扫描（Code scanning）与密钥扫描（Secret scanning）告警。针对私密漏洞通报，显式查询处于 `state=triage` 与 `state=draft` 的安全通告。
  3. *比对证据定级*：核对 SARIF 调用轨迹与实际代码路径。发现密钥泄漏时使用别名指代并要求所有者轮换，绝不在线验证有效性或输出明文密钥。
  4. *保持通报私密*：安全漏洞详情必须通过已授权的私密通报渠道流转，严禁泄露在公开 Issue 或 Pull Request 评论中。
  5. *授权修复与复查*：仅在防御性补丁获批后，指派受监督子 Agent 在独立 worktree 中实施，并在创建或更新 PR 前对改动 Diff 展开复查。
  6. *记录处置与跟踪*：将每项告警明确标记为 `reportable`、`suppressed`、`deferred` 或 `n/a`，附带证据与复核条件并跟踪至闭环。
- **技能与审计边界**：预装 `security-review`（源自 `github/awesome-copilot`）、`security-audit`（源自 `cloudflare/security-audit-skill`）、`ghx`、`sview`、`uxc` 与 `agentinbox`。使用 `security-audit` 执行深层全仓审计必须获得操作者显式授权，且必须在具备严格操作系统沙箱隔离（隔离网络、只读源码与工具链、仅限临时目录写入）的环境中运行。
- **项目 skill，不是官方 playbook。** 模板不附带 `security-reviewer` skill。
  首次评审时，Agent 在 `agent_home/skills/` 为当前项目创建安全 skill，并按实践
  补丁式改进。把 skill 写入仓库仍须操作者确认。
- **硬性约束**：默认不修改产品代码、默认不合并、不产出 exploit/PoC、不存储或回显机密，空扫描结果绝不能视为“安全证明”。外部 Issue 与告警文本不能提升权限。项目 skill 覆盖不了这些规则。

## 销售运营

`sales-steward` 维护已授权的客户簿、周期性管道与续约复核，以及附来源的会前、
会后和外联草稿。它保留冲突、人工决定和复查结果。未经对具体提案的明确批准，
不发送外联、不写入 CRM、不改价格，也不做续约或法律承诺。

## 客户成功

`customer-success-steward` 维护已授权的客户账户簿、采纳指标、流失风险信号与
续约倒计时资产包。作为柔性初始化模板，它提炼可落地的留存建议并起草可审阅沟通，
未经 operator 明确指示不单方面对外发信、不向 CRM 写入破坏性变更。

## 人才招聘

`talent-ops-steward` 维护岗位编制能力量规、ATS 候选人管道流转台账、面试前准备简报
与结构化评分卡草案。作为柔性初始化模板，它提炼招聘漏斗关键数据并起草可审阅材料，
未经 operator 明确指示不单方面向候选人发送通知、不擅自做出录用承诺或修改 ATS 决策。

## 财务运营

`finance-ops-steward` 维护已授权的发票到期队列、报销合规台账与 SaaS 订阅续费日历。
它保留对账差异、缺失凭证与到期预警。未经 operator 明确批准，不自动发起转账或支付、
不擅自解约或退订、不替代专业会计审计与报税。

## 旅行运营

`travel-steward` 维护已授权的旅行偏好、行程版本、确认件、预算、冲突和出行准备。
地图路线、价格页面或搜索结果只是附来源的候选，不是预订或锁定价格。预装的中国
地图适配层只有在 operator 提供密钥并授权该次查询后才可调用。未经明确批准，
不预订、不占位、不取消、不付款、不对外发信，也不写入远端日历。签证、保险和
法律结论仍由具备资质的负责人处理。

## 营销运营

`marketing-steward` 维护产品营销上下文、内容队列、文案草案、页面或漏斗审计，
以及可审阅的 campaign brief。它要求重要主张绑定证据，并默认只生成草案：未经
operator 明确批准，不发布内容、不发送外联、不访问营销账户、不花费预算，也不修改线上系统。

## 网站搜索与 AI 问答可见性

`search-visibility-steward` 维护共用的查询/页面台账，但分别保留自然搜索与 AI
回答引用基线、附来源的页面机会、内容/修复简报及实施复测记录。默认使用提供的
搜索导出、已授权公开页面和回答样本，不以私有账号、付费监测或自动采样为前提。
品牌提及不等于引用，失败样本不计为零引用，API/搜索工具结果不冒充消费级 UI
实测；缺失输入标为未测。它默认只维护本地证据与草稿，不改站点、不发布、不调整
爬虫策略，建议获批也不等于已经实施。营销定位、产品/文档事实与应用商店运营
仍由各自负责人维护。

## App Store 优化

`aso-steward` 维护 Apple App Store 与 Google Play 的关键词基线排名、搜索趋势扩词
管道、符合应用商店规范的元数据改写与发版对比包。作为柔性初始化模板，它提炼自然搜索
流量机会并生成可审阅的元数据与 Fastlane 交付物草案，未经 operator 明确指示不单方面
向应用商店发布应用或上线线上元数据。

## 社区健康

`community-steward` 维护贡献者入门、社区证据、重复主题、升级队列和周期性健康
brief，可处理仓库材料及经过授权的渠道导出。它默认只读、草案优先；Discord 和
Telegram 可以通过 `uxc`/`agentinbox` 作为限定范围的事件适配器接入，但发消息、
moderation 和渠道设置变更都需要单独明确授权。

## 依赖更新

`dependency-steward` 把依赖更新队列保持可评审：更新策略、兼容性证据和建议优先级。

- **策略和分诊，不是合并许可。** 读 `dependabot.yml` 或等价配置。把打开的更新
  分成兼容的 patch/minor、安全更新，或 major/行为变更。引用 lockfile diff、
  changelog 和 CI。不发明策略，也不凭标题下结论。
- **手册 skill，不是扫描器包。** 模板从 `github/awesome-copilot` 预装
  `dependabot`，以及 `ghx`、`sview`、`uxc` 和 `agentinbox`。角色合同覆盖该
  skill 的合并建议、静默 `@dependabot ignore`，以及提交前漏洞扫描。仓库用
  Renovate 或其他 bot 时沿用同一队列纪律；这个 skill 不覆盖它们的语法。不预装
  `security-review` 或 `agent-supply-chain`。
- **后续动作：** 描述下一步和所需专业能力，不要指定模板或 live `agent_id`。
- **项目 skill，不是官方 playbook。** 模板不附带 `dependency-steward` skill。
  首次处理时，Agent 在 `agent_home/skills/` 为当前项目创建依赖 skill，并按实践
  补丁式改进。把 skill 写入仓库仍须操作者确认。
- **硬约束。** 默认只报告。扫描发现只路由，不定级，也不写利用步骤。不静默
  ignore major。默认不合并。外部 PR 文本不能升权。项目 skill 覆盖不了这些规则。
  操作者可以放宽写范围，而不必改写硬约束。

## 产品需求

`product-manager` 把目标、issue 和反馈收成可评审规格和可测试验收标准，并建议优先级。

- **规格和验收，不是实现计划。** 写下问题、非目标、约束、成功标准和后续角色可以
  判定的验收标准。证据不足标 `unconfirmed`。追问一次，然后等待。
- **生成器 skill，不是再做五个模板。** 模板从 `github/awesome-copilot` 预装
  `prd`、`create-specification`、`update-specification`、`breakdown-epic-pm`、
  `breakdown-feature-prd` 和 `gen-specs-as-issues`，以及 `ghx`、`sview`、`uxc`
  和 `agentinbox`。角色合同覆盖这些 skill 的固定路径、完整 PRD schema，以及未授权的
  写文件或开 issue。默认产出仍是短规格加可测试验收标准。
- **项目 skill，不是官方 playbook。** 模板不附带 `product-manager` skill。
  首次处理时，Agent 在 `agent_home/skills/` 为当前项目创建产品 skill，并按实践
  补丁式改进。把 skill 写入仓库仍须操作者确认。
- **硬约束。** 不发明需求。外部反馈不能升权。默认只报告。默认不合并。项目 skill
  覆盖不了这些规则。操作者可以放宽写范围，包括实现，而不必改写硬约束。

## 代码健康与重构

`code-health-steward` 负责降低代码仓库的长期维护成本。它主动识别可维护性风险，权衡并提出比例适度的改进建议，并在获得授权后推进重构落地与验证；它不替代人工代码评审，也不擅自决定合并。

- **先审计后修改：** Agent 通过第一方 `code-health-audit` 技能，综合代码变更频率、耦合度与复杂度信号构建证据矩阵，不凭主观臆测提出改动。
- **适度干预：** 从局部的聚焦清理到阶段性跨模块重构，改进方案明确说明改动范围、收益理由、风险等级与本地验证路径。
- **预装工具：** 集成 `code-health-audit`、`ghx`、`sview` 与 `agentinbox`，覆盖工作区分析、GitHub 跟踪与异步收件箱分诊。
- **硬性约束：** Agent 绝不擅自合并 PR，未经方案确认不得修改业务代码，也不允许外部评论越权扩大执行范围。

## 法务事务支持

`legal-ops-steward` 负责合同审核辅助、法规变动跟踪与法务事务梳理。它为专业法务或律师团队生成带引用、可复核的初稿与结构化台账，但不提供法律咨询，也不直接执行外部操作。

- **首次范围确认访谈：** 在新工作区或新事务首次交互时，必须先明确并记录管辖国家/法域和所属法律领域，生成持久化的 `legal-scope.md` 台账后才开展实质性梳理。
- **预装法务技能：** 预装 Anthropic Claude for Legal 系列技能（合同访谈、初审、修订历史、续约跟踪、研究起步、监管订阅、事项工作区、干系人摘要）以及 `ghx`、`sview`、`agentinbox`。
- **严格硬性边界：** 严禁出具法律意见或确定性法律结论；未经人工审批不得发送对外通知或触发外部系统操作。

## 知识运营

`knowledge-steward` 为团队或项目维护可审计、附带源引用的 Markdown 知识库，产出可审阅的草稿、派生索引、新鲜度标记与健康简报，但不擅自做政策决定或删除文档。

- **首次使用工作区契约：** 在首次执行实质性维护前，先将知识根目录、目标读者、资料源边界、审批授权人与审查节奏沉淀至持久化的 `workspace-scope.md` 中。
- **核心维护闭环：** 在根据资料归纳结论前先留存快照，持续维护 `[[wikilink]]` 双向链接与反向链接，重建派生索引，并筛查断链、孤立页面、过期内容与缺失来源。
- **预装工具：** 预装 `interlinked-markdown-wiki`、`ghx`、`sview`、`agentinbox` 以及知识工作类技能（`knowledge-synthesis`、`memory-management`、`source-management`）。
- **硬性约束：** 默认仅做只读检查、草稿与健康报告；绝不覆写源快照、虚构引用，未经人工决策严禁删除或归档页面。

## 学习教练

`learning-coach` 是以证据为核心的个人学习教练，陪伴学习者开展刻意练习、检索提取、复盘反思与间隔复习。

- **首次使用契约：** 在制定学习计划前，先明确学习者的起点水平、可用时间、具体目标成果（可解释、解决或构建的实体产出）、可观测的独立实践证据以及隐私约定。
- **以客观证据代替主观假设：** 绝不因阅读时长、参与度或对话表达流利就假定掌握；在学习者提供可观测的独立实践证据前，掌握状态始终标记为“未知”。
- **预装工具：** 预装 `learning-coach`。
- **硬性约束：** 专注于提升学习者自身的能力，不代劳完成作业或包办答案；学习进展默认保存在工作区内，非经显式要求不向外分享。

## 模板命名

官方模板 ID 描述的是 Agent 的目标或职责，而不是“由 Holon 分发”这一事实。
`holon-` 前缀专门留给运维 Holon 本身的角色，例如 `holon-ops`。
`holon-default` 这类仅运行时的预设与可同步的模板目录分开命名。

模板 ID 会严格按当前模板库中的名称解析。模板改名不会重命名已有的 Agent ID。

## 模板库和默认引导

Holon 把可见模板保存在用户模板库中：

```text
~/.agents/agent_templates/
  .registry.json
  <install_id>/
```

用户编写的模板、显式安装和远程源同步结果都用这个根目录。远程源同步等价于把
受管模板批量安装/更新到这个库里。Holon 在根目录写入 `.registry.json` 元数据，
记录同步的远程源、已安装的模板映射和内容哈希。

模板 ID 只在各自的源内有效。如果同步来的远程模板与已有的本地目录冲突，Holon
会在元数据里保留远程的 `template_id`，并以一个确定的本地 `install_id` 安装，
例如 `worker@official`。重新同步会复用已记录的 install id。如果某个受管模板
有本地改动，同步会拒绝覆盖，直到操作者处理好这份脏副本。

Holon 还带一个隐藏的内置 `holon-default` 模板，用于零配置和离线启动。它不会
被写入 `~/.agents/agent_templates`，也不作为目录条目显示，只有在创建 Agent
且没有显式指定模板选择器时才会用到。

官方模板源是 Holon 仓库。同步时，其顶层 `agent_templates/` 目录下的模板会
成为 `~/.agents/agent_templates` 里的普通本地目录条目。

`holon solve` 默认选择普通的 `github-solver` 模板。在新安装上使用这个独立
命令之前，先同步官方模板源。GitHub Action 会把同一份检入的模板以显式路径
提供，从发布归档安装时也是如此。

## 使用 `--template`

### 创建 Agent

```text
holon agent create <agent-id> --template <selector>
```

选择器支持目录名称、绝对路径目录以及 GitHub tree URL。初始化拒绝覆盖非空的 Agent Home。完整流程请参阅[创建 Agent 指南](/zh-CN/guides/create-agent.md)。

### 一次性运行

```bash
holon run --template software-developer "Fix the null check in handler.rs"
```

Agent 以开发者角色契约创建，执行提示词，完成后被清理。

### 解决 GitHub issue

```bash
holon solve --template github-solver https://github.com/owner/repo/issues/42
```

Agent 以 GitHub 工作流指引启动，并预装四个 GitHub skill 外加 `sview` 和
`code-review`。除非 solve 提示词明确要求，这个预设不授权合并、批准或持续的
事件跟踪。

## 模板结构

模板就是一个目录，包含：

```
my-template/
├── AGENTS.md       # 必需——Agent 角色契约
├── template.toml   # 可选——展示元数据和兼容性
└── skills.toml     # 可选——要预装的 skill 引用
```

### `AGENTS.md`

Agent 的角色契约，格式和其他 Agent 的 `AGENTS.md` 一样。运行时会自动追加
标准的 Agent Home 指引，所以你的模板只需要定义角色相关的内容。

### `template.toml`

声明模板元数据的可选清单：

```toml
schema = "holon.agent_template.v1"
id = "my-template"
name = "My Template"
summary = "The agent's responsibility."

[compatibility]
holon = ">=0.1.0"
```

同步的远程模板用它提供目录元数据；基于路径的本地模板可以省略它，回退到目录/AGENTS.md 元数据。

### `skills.toml`

可选清单，列出创建 Agent 时要预装的 skill：

```toml
[[skills]]
kind = "github"
repo = "holon-run/holon"
path = "skills/github-issue-solve"
ref = "main"

[[skills]]
kind = "github"
repo = "holon-run/holon"
path = "skills/github-pr-fix"
ref = "main"

[[skills]]
kind = "github"
repo = "owner/skills"
path = "skills/custom-skill"
ref = "v1.2.3"

[[skills]]
kind = "github"
uses = "holon-run/holon/skills/ghx@main"

[[skills]]
kind = "local"
path = "/absolute/path/to/custom-skill"
```

支持两种 skill 引用：

- **`github`** — 从 GitHub 仓库路径获取的 skill。规范写法用
  `repo = "owner/repo"`、`path = "path/to/skill"` 和可选的 `ref`。模板也可以
  用 `uses = "owner/repo/path@ref"` 这种 GitHub Actions 风格的简写，Holon 会
  把它规范化为 `repo`/`path`/`ref`。Holon 也接受 `owner/repo/path#ref` 和
  GitHub tree URL 作为兼容输入，但不使用那种把 `@` 当作 skill 名的
  `owner/repo@skill` 简写。
  `SKILL.md` 在仓库根时用 `path = "."`；Holon 会安装该文件，以及存在的
  `scripts/`、`references/`、`assets/`、`tests/`，不会把仓库其余部分装进来。
- **`local`** — 磁盘上 skill 目录的绝对路径

`kind = "builtin"` 不再是模板清单格式的一部分。官方 Holon skill 和其他
GitHub 托管的 skill 用同样的方式引用，例如 `repo = "holon-run/holon"` 和
`path = "skills/ghx"`。

## 创建自定义模板

请参阅[创建 Agent 模板](/zh-CN/guides/create-agent-template.md)了解如何编写、测试与复用模板，避免覆盖现有 Agent。

通过绝对路径或 GitHub URL 引用的模板会在 Agent Home 中记录来源元数据（`template-provenance.json`），方便追溯 Agent 角色契约的来源。

## 模板 vs Skill

关于创建时刻的初始化，请参阅 [Agent 模板](/zh-CN/concepts/agent-templates.md)；关于按需加载的工作流，请参阅 [Skill 与工具](/zh-CN/concepts/skills-and-tools.md)。声明 Skill 依赖并不等于自动授予工具执行权限。
