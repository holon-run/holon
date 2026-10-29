---
title: 搭建 AI Agent 生态研究员：跨多开源项目的持续深度调研
summary: 结合 Web GUI 与 CLI 操作，复用 Research Steward 模板与多 Workspace 机制。深入剖析 Agent 自动维护知识库的底层原理，并介绍移动端随手转发链接、异步生成调研报告的日常交互。
order: 85
---

# 搭建 AI Agent 生态研究员：跨多开源项目的持续深度调研

在跟踪快速演进的技术领域（例如 AI Agent 框架、运行时与协议）时，单纯依赖一次性的模型对话或网页搜索往往难以胜任：

- **缺乏一手源码证据**：网页抓取与二次解读容易泛化甚至过时，真正的关键细节隐藏在开源项目的架构实现、提交历史、Release 契约和测试用例中；
- **多项目上下文混乱**：技术调研往往需要横向对比多个独立的开源项目（例如不同的 Harness、运行时调度器或通信协议）。如果将 Agent 限制在单个目录，它无法跨仓库对比；如果让 Agent 在被测仓库内随意记录笔记，又会污染原始代码库；
- **缺乏长期资产沉淀**：一次会话结束，研究过程和对比矩阵便丢失了，无法做增量跟踪和基线复核。

本指南演示如何利用 Holon 的 **Research Steward（研究管家）模板** 理念以及 **多工作区挂载（Multi-Workspace Attach）机制**，搭建一个名为 `ecosystem-researcher` 的长效技术调研 Agent。

在这个体系中：
- **双模操作**：创建 Agent、挂载工作区等配置，既可通过直观的 **Web GUI** 在浏览器中点选完成，也可通过 **CLI 命令行** 快速执行；
- **全自动维护**：日常使用时你无需手动建立目录或编写配置文件，Agent 会全自动维护其私有知识库，本文将详细拆解其**背后的结构化设计原理**；
- **日常随手交互**：支持在手机端或桌面端直接向 Agent 转发开源项目链接或文档，由 Agent 在后台异步完成深度调研并推送简报。

## 核心心智模型

在动手前，先理解本场景涉及的核心设计边界：

1. **研究角色契约（Role Contract）与证据准则**：
   研究类 Agent 的核心职责是“证据收集与决策简报”，而不是“越权替人类做决策”或“修改产品代码”。复用 `research-steward` 模板可以为 Agent 注入严格的证据规则：
   - 区分**事实**（有明确源码 commit/行号或官方文档支撑）、**判断**（基于多方证据的架构推论）与**建议**（对内部产品的行动意见）；
   - 一手来源优先，不将未经引证的搜索摘要直接作为结论；
   - 结论默认只对基线 Commit 有效，增量更新采用追加记录，不随意推倒重来。

2. **全自动维护与底层沉淀原理（Autonomous Maintenance）**：
   使用者不必扮演“文件管理员”。在实际使用中，知识库的目录初始化、项目登记索引表维护以及架构报告归档，都是由 Agent 根据指令或转发的链接**自主驱动、全自动完成**的。了解其背后的知识库目录规范，有助于理解 Agent 如何维持稳定的长期记忆。

3. **Agent 专属 Home 与外部代码工作区解耦**：
   - **`agent_home`**：Agent 的独立私有空间，用于保存该 Agent 的长期知识库、调研登记表（`registry.yaml`）、生态分类法（`taxonomy.md`）和历史产物；
   - **挂载外部 Workspace**：将多个外部开源代码仓库作为只读工作区挂载进来。Agent 可以平滑切换执行根进行跨项目源码审查，而不会把任何研究过程产物写入外部代码库，保持仓库绝对纯净。

4. **多端触达与异步任务机制**：
   依托 Holon 的持久化守护进程与 WorkItem 机制，调研任务支持真正的异步执行。你在移动端随手分享一个 GitHub 链接，便可关闭屏幕离开；Agent 在后台持续抓取、分析、比对，完成后自动呈递交付简报。

## 前置条件

- 本地已安装并启动 Holon 守护进程（默认启用内嵌 Web GUI，可通过浏览器访问 `http://localhost:8080`；终端中 `holon` CLI 亦可用）。
- 本地磁盘已克隆或准备好若干需要被观察的开源项目仓库（例如本文示例中的两个项目）：
  - `/home/user/src/github.com/org/project-a`
  - `/home/user/src/github.com/org/project-b`
  *(若仅做网络调研或文档对比，可直接使用后续介绍的链接转发模式)*。

## 操作步骤

### 1. 从研究模板创建 Agent 并配置专业技能

Holon 提供了面向深度调研场景的 Agent 模板 `research-steward`，预置了严谨的证据收集契约。你可以选择使用 **Web GUI** 或 **CLI 命令行** 完成创建。

#### 方式 A：通过 Web GUI 操作

1. **打开 Web GUI**：在浏览器中访问 Holon Web GUI（例如 `http://localhost:8080`）。
2. **从模板创建 Agent**：
   - 在左侧导航栏点击 **Templates**（`/templates`）；
   - 在模板列表中找到 `research-steward`（如未安装，可点击 **Install Template** 输入对应源进行安装）；
   - 点击 **Create Agent** 按钮，在弹出表单中输入：
     - **Agent ID**：`ecosystem-researcher`
     - **名称**：`生态调研员`
   - 点击确认创建。
3. **启用调研必备的 Skill**：
   - 前往导航栏的 **Skills**（`/skills`）页面；
   - 找到并为 `ecosystem-researcher` 开启以下专业技能：
     - `ghx`：用于精准读取 GitHub 仓库的 Release、Issue、PR 与提交历史；
     - `sview`：提供结构化、只读的文件审查与切片浏览能力；
     - `citation-verifier`：核验引文真实性与来源对应关系；
     - `writing-clearly-and-concisely`：保证研究简报风格清晰、精炼，避免大模型套话。

#### 方式 B：通过 CLI 终端操作

在终端中依次运行以下命令：

1. **创建研究 Agent 实例**：

   ```bash
   holon agent create ecosystem-researcher --template research-steward --name "生态调研员"
   ```

2. **按需启用调研必备的 Skill**：

   ```bash
   holon skills enable ghx --agent ecosystem-researcher
   holon skills enable sview --agent ecosystem-researcher
   holon skills enable citation-verifier --agent ecosystem-researcher
   holon skills enable writing-clearly-and-concisely --agent ecosystem-researcher
   ```

无论通过 GUI 还是 CLI 创建，该 Agent 根目录下的 `AGENTS.md`（位于 `~/.holon/agents/ecosystem-researcher/AGENTS.md`）均已写入长效角色契约。

### 2. 挂载多个开源项目工作区（Multi-Workspace Attach）

若需让 Agent 深入本地源码进行 AST 级架构拆解，可通过工作区挂载将其与外部项目连接起来。

#### 方式 A：通过 Web GUI 操作

1. 在 Web GUI 左侧选择 `ecosystem-researcher`，进入该 Agent 的详情视图；
2. 点击 **Settings** 或 **Workspaces** 面板，点击 **Attach Workspace**；
3. 分别选择或输入本地已克隆的开源项目绝对路径（如 `/home/user/src/github.com/org/project-a` 与 `/home/user/src/github.com/org/project-b`），点击挂载；
4. 挂载完成后，可在工作区列表中清晰看到这两个关联仓库，而 Agent 的主执行根依然稳妥保持在其私有的 `agent_home`。

#### 方式 B：通过 CLI 终端操作

1. **将多个外部开源项目挂载至该 Agent**：

   ```bash
   # 挂载项目 A
   holon workspace attach --agent ecosystem-researcher /home/user/src/github.com/org/project-a

   # 挂载项目 B
   holon workspace attach --agent ecosystem-researcher /home/user/src/github.com/org/project-b
   ```

2. **验证 Agent 的工作区挂载状态**：

   ```bash
   holon agent get ecosystem-researcher
   ```

   输出中的 `attached_workspaces` 会列出关联的所有外部路径。

> **注意**：`workspace attach` 仅扩展 Agent 对外部代码目录的访问授权，不会改变默认执行根。Agent 日常活动依然以私有的 `agent_home` 为核心。

### 3. 背后的原理：Agent 自动维护的专属知识库体系

> **重点机制说明**：
> 在实际日常使用中，**你完全不需要手动创建这些目录或逐行编写 YAML 配置文件**！
> 当你向 Agent 下达研究目标或转发项目链接时，Agent 会根据角色契约**全自动初始化目录结构、自动填充对象索引表并增量归档调研报告**。
> 本节展示其内部结构，是为了让你理解**背后的沉淀原理**——为什么该 Agent 能够在经历多次会话与长期运行后，依然条理清晰、记忆不丢失。

在 Agent 专属的 `agent_home` 中，Agent 会自动组织并维持如下知识库布局：

```text
~/.holon/agents/ecosystem-researcher/
├── AGENTS.md                  # 长效角色契约与证据准则
├── work/agent-ecosystem/      # 核心研究知识库（Agent 自主维护）
│   ├── taxonomy.md            # 生态分类法（Agent 自动归类与迭代演进）
│   ├── registry.yaml          # 被观察对象的唯一登记索引表（自动增删与状态记录）
│   ├── projects/              # 各项目的独立长期分析归档
│   │   ├── project-a/
│   │   │   ├── metadata.yaml  # 源码路径、基线 commit、仓库元数据
│   │   │   ├── architecture.md# 架构拆解与核心模块证据
│   │   │   └── updates.md     # 随版本发布的增量跟踪记录（只追加不重写）
│   │   └── project-b/
│   └── matrices/              # 跨项目横向比对矩阵（按技术专题汇总）
```

#### 索引登记表规范（`registry.yaml` 原理）

当 Agent 获悉新的观察对象时，它会自动在 `registry.yaml` 中维护条目：

```yaml
meta:
  initialized: 2026-10-29
  description: "AI Agent 生态研究对象索引"

objects:
  - slug: project-a
    name: "Project A Runtime"
    category: "agent_runtime"
    status: "monitoring"
    upstream: "https://github.com/org/project-a"
    sources:
      - role: primary
        local_path: "/home/user/src/github.com/org/project-a"
        baseline_commit: "a1b2c3d4..."
    research_path: "projects/project-a"

  - slug: project-b
    name: "Project B Harness"
    category: "harness"
    status: "monitoring"
    upstream: "https://github.com/org/project-b"
    sources:
      - role: primary
        local_path: "/home/user/src/github.com/org/project-b"
        baseline_commit: "e5f6g7h8..."
    research_path: "projects/project-b"
```

通过将状态固化在结构化文件中，即便对话历史因模型上下文轮转而被截断，Agent 也能在下一次唤醒时直接读取登记表，精确找回历史基线。

### 4. 发起一次跨项目的深度架构对比任务

通过 Web GUI 对话框或 CLI / TUI 向 Agent 发送横向调研任务：

**任务指令示例（直接发送给 Agent）**：

> “请对已挂载的 project-a 与 project-b 进行一次横向技术调研：
> 1. 分别审查两者在 Agent 进程调度与上下文中断恢复机制上的实现；
> 2. 给出关键代码位置（包括 commit 与行号范围）；
> 3. 比较两者在长任务持久化设计上的优缺点；
> 4. 将对比结果沉淀到 `work/agent-ecosystem/matrices/task-resumption.md`，并更新两项目的 `architecture.md`。”

**Agent 收到指令后的自动化闭环**：
1. **自动切换执行根**：Agent 调用 `SwitchWorkspace` 将执行上下文临时切换到 `project-a`，利用只读工具（`sview` 或 `rg`）分析代码，摘录确凿的行号与提交 SHA；
2. **交叉审查对照组**：Agent 再次切换到 `project-b`，审查其对应的调度抽象与恢复逻辑；
3. **自动写回私有知识库**：分析完毕后，Agent 切回 `agent_home`，将比对结论自动写入 `matrices/task-resumption.md`，并同步丰富单项目的 `architecture.md`；
4. **呈递精简交付简报**：在对话界面中返回结论概要与引证清单，包含清晰的事实与架构推论区分。

### 5. 验证调研产物与隔离效果

1. **查看调研成果**：
   - **Web GUI 方式**：在 Web GUI 的对话结果中直接查看 Agent 回复的精炼简报；点击关联产物链接或在左侧文件浏览器中打开 `work/agent-ecosystem/matrices/task-resumption.md` 阅读完整报告；
   - **CLI 方式**：运行命令查看生成的内容：
     ```bash
     cat ~/.holon/agents/ecosystem-researcher/work/agent-ecosystem/matrices/task-resumption.md
     ```

2. **核验外部代码库绝对纯净**：
   检查挂载的被测项目 Git 状态：
   ```bash
   git -C /home/user/src/github.com/org/project-a status
   git -C /home/user/src/github.com/org/project-b status
   ```
   两者输出均为 `working tree clean`。这证明 Agent 的全部研究过程产物都被严格隔离在自己的私有空间中，对被测源码没有任何污染。

---

## 日常交互场景：随手转发项目与移动端异步调研

除了在电脑前进行多项目源码的深度审查外，技术生态调研往往来自于日常碎片化浏览时的偶然发现。在移动端场景下，Holon 的长效异步特性体现得尤为充分。

### 移动端随手分享链接

当你通过手机（无论是使用 [iOS 客户端](./connect-ios-client.md)、[Android 客户端](./connect-android-client.md) 还是移动端网页浏览器）刷社交平台、技术论坛或 GitHub 时，一旦看到一个值得关注的新开源项目或深度架构文章：

1. **直接分享或发送给 Agent**：
   在手机中点击分享，或在 Holon 对话界面中直接粘贴链接并附带简短诉求：
   > “调研一下这个新项目：https://github.com/example/novel-runtime ，重点看它的调度机制和定位，与我们已有的生态库进行对比。”

2. **无需驻留，离线异步执行**：
   - 发送后即可直接锁屏或切换应用。因为 Holon 是运行在主机上的长效守护进程，任务由底层的 WorkItem 驱动，不需要客户端保持连接；
   - Agent 收到指令后，会自动调用网络工具（如 `WebFetch`、`WebSearch`）和 `ghx` 抓取仓库的主页信息、README、Release 契约和架构文档；
   - Agent 自动将其登记到 `registry.yaml` 索引中，并在 `projects/novel-runtime/architecture.md` 中生成第一期技术简评。

3. **随时随地查阅交付简报**：
   - 稍后重新打开手机客户端或电脑端 Web GUI，Agent 的研究报告已在等待你审阅：包含该项目的定位分类、核心创新点、潜在局限性以及与现有观察项目的异同；
   - 如果初评结果显示该项目非常有价值，你回到工作台后只需将其实际克隆到本地并执行一次 `workspace attach`，Agent 便能在现有分析基础上，无缝进入该项目的本地源码进行 AST/Commit 级的深层研读。

---

## 进阶技巧：长期增量跟踪与自动化巡检

当生态中的开源项目持续发布新版本时，无需每次重新进行全量调研：

- **基线 Commit 锁定**：始终以 Agent 记录在 `registry.yaml` 中的 `baseline_commit` 为基准。只有当你明确要求升级基线时才执行比对更新。
- **配置定时巡检**：
  利用 Holon 的定时器机制（参阅[调度 Agent 工作](./schedule-agent-work.md)），无论在 Web GUI 还是 CLI 中，均可为 Agent 设置每周或每天的定时唤醒任务。Agent 会自动检查被观察项目的最新 Release 或提交历史，仅针对破坏性变化生成增量记录（`updates.md`），形成活态技术雷达。

## 相关内容

- [使用 Web GUI](./use-web-gui.md)：浏览器端 Agent 管理、模板安装与产物浏览。
- [连接移动端客户端（iOS）](./connect-ios-client.md) / [Android 客户端](./connect-android-client.md)：在移动设备上随时与 Agent 交互并查阅产物。
- [在项目工作区或 worktree 中工作](./work-with-workspaces.md)：工作区挂载、执行根切换与 Worktree 隔离机制。
- [从模板创建 Agent](./create-agent.md)：了解 Agent 模板初始化流程与内置模板。
- [调度 Agent 工作](./schedule-agent-work.md)：配置单次或周期性定时器执行长期巡检。
- [获取与搜索网络内容](./use-web-tools.md)：结合网络搜索与文档抓取补全开源项目的一手资料。
