---
title: 搭建 AI Agent 生态研究员：跨多开源项目的持续深度调研
summary: 复用 Research Steward 模板与多 Workspace 机制。深入剖析 Agent 自动维护知识库的底层原理，展示让 Agent 自动克隆挂载仓库、精简指令交互，以及移动端随手转发链接的异步调研工作流。
order: 85
---

# 搭建 AI Agent 生态研究员：跨多开源项目的持续深度调研

在跟踪快速演进的技术领域（例如 AI Agent 框架、运行时与协议）时，单纯依赖一次性的模型对话或网页搜索往往难以胜任：

- **缺乏一手源码证据**：网页抓取与二次解读容易泛化甚至过时，真正的关键细节隐藏在开源项目的架构实现、提交历史、Release 契约和测试用例中；
- **多项目上下文混乱**：技术调研往往需要横向对比多个独立的开源项目（例如不同的 Harness、运行时调度器或通信协议）。如果将 Agent 限制在单个目录，它无法跨仓库对比；如果让 Agent 在被测仓库内随意记录笔记，又会污染原始代码库；
- **缺乏长期资产沉淀**：一次会话结束，研究过程和对比矩阵便丢失了，无法做增量跟踪和基线复核。

本指南演示如何利用 Holon 的 **Research Steward（研究管家）模板** 理念以及 **多工作区挂载（Multi-Workspace Attach）机制**，搭建一个名为 `ecosystem-researcher` 的长效技术调研 Agent。

在这个体系中：
- **模板开箱即用**：从研究模板创建的 Agent 默认装配了调研所需技能，无需繁琐的手动配置；
- **全自动维护与挂载**：无需提前手动克隆仓库，直接让 Agent 自动克隆并挂载工作区；日常使用无需手动建目录或写配置，Agent 全自动维护私有知识库，本文将剖析其**背后的结构化沉淀原理**；
- **极简指令交互**：代码定位准则、事实推论区分与产物归档路径已内置在长效契约中，日常发任务只需一两句自然表达；
- **日常随手交互**：在手机端或桌面端直接向 Agent 转发开源项目链接或文档，由 Agent 在后台异步完成深度调研，点击产物链接即可阅读。

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
   - **挂载外部 Workspace**：将多个外部开源代码仓库作为工作区挂载进来。Agent 可以平滑切换执行根进行跨项目源码审查，而不会把任何研究过程产物写入外部代码库，保持被观察项目绝对纯净。

4. **多端触达与异步任务机制**：
   依托 Holon 的持久化守护进程与 WorkItem 机制，调研任务支持真正的异步执行。你在移动端随手分享一个 GitHub 链接，便可关闭屏幕离开；Agent 在后台持续抓取、分析、比对，完成后自动呈递交付简报。

## 前置条件

- 本地已安装并启动 Holon 守护进程（Web GUI 默认在 `http://localhost:8080`，终端 `holon` CLI 亦可用）。
- **无需提前在本地克隆代码仓库**：你可以直接将 GitHub 仓库地址发给 Agent，由 Agent 自行完成克隆并添加为工作区；若本地已有项目目录，也可直接指定路径。

## 操作步骤

### 1. 从研究模板创建 Agent

Holon 提供了面向深度调研场景的 Agent 模板 `research-steward`，预置了严谨的证据收集契约。

> **默认携带技能说明**：
> `research-steward` 模板在创建时已默认包含技术调研所需的技能集（如 `ghx` 用于获取 GitHub 数据与源码检索、`sview` 用于结构化只读代码审查、`writing-clearly-and-concisely` 保证简报精炼严谨等）。**创建完成后即可直接使用，无需再单独配置或手动启用这些技能**。

#### 方式 A：通过 Web GUI 操作

1. **打开 Web GUI**：在浏览器中访问 Holon Web GUI（例如 `http://localhost:8080`）。
2. **创建 Agent**：
   - 在左侧导航栏点击 **Templates**（`/templates`）；
   - 在模板列表中找到 `research-steward`；
   - 点击 **Create Agent** 按钮，在弹出表单中输入：
     - **Agent ID**：`ecosystem-researcher`
     - **名称**：`生态调研员`
   - 点击确认创建。

#### 方式 B：通过 CLI 终端操作

在终端中运行一行命令完成创建：

```bash
holon agent create ecosystem-researcher --template research-steward --name "生态调研员"
```

创建完成后，该 Agent 根目录下的 `AGENTS.md`（位于 `~/.holon/agents/ecosystem-researcher/AGENTS.md`）已完整载入长效角色契约与证据准则。

### 2. 添加多个开源项目工作区（Workspaces）

若需让 Agent 深入分析本地源码，可通过多工作区机制将目标项目连接进来。

#### 方式 A：直接在对话中让 Agent 自动克隆并挂载（推荐）

目前 Web GUI 尚未提供独立的 Attach Workspace 图形按钮，最简便直接的做法是在对话框中直接告诉 Agent：

> “帮我把 https://github.com/org/project-a 和 https://github.com/org/project-b 克隆到本地，并添加为你的工作区。”

Agent 收到后会自动在本地拉取仓库，并通过 Holon 内置的工作区工具自动完成挂载。

#### 方式 B：通过 CLI 挂载本地已有目录

如果本地已有克隆好的代码仓库，也可以在终端中直接挂载：

```bash
holon workspace attach --agent ecosystem-researcher /path/to/project-a
holon workspace attach --agent ecosystem-researcher /path/to/project-b
```

验证挂载状态：
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

向 Agent 下发任务非常简单，**不需要在每次发送时重复写大段格式与证据约束**，因为这些规则已经固化在 Agent 的 `AGENTS.md` 中：

**任务指令示例（直接在对话框中发送）**：

> “调研一下 project-a 与 project-b 在进程调度与中断恢复机制上的实现差异，做个对比。”

**Agent 收到指令后的自动化闭环**：
1. **自动切换执行根深入源码**：Agent 调用 `SwitchWorkspace` 临时切换到 `project-a` 与 `project-b`，利用 `sview` 等只读工具检索关键抽象，记录代码行号范围与 Commit SHA；
2. **自主沉淀比对矩阵**：根据契约，Agent 分析完成后切回 `agent_home`，将结论写入 `work/agent-ecosystem/matrices/task-resumption.md`，并同步更新单项目的 `architecture.md`；
3. **呈递精简交付简报**：在对话中返回结构化摘要与结论，并附带产物链接。

### 5. 验证调研产物与隔离效果

1. **查看调研成果**：
   任务完成后，Agent 会在对话中输出结论摘要。
   - **直接阅读完整报告**：点击简报中的**关联产物链接**，或者在 Web GUI 左侧文件浏览器中打开 `work/agent-ecosystem/matrices/task-resumption.md` 即可查阅完整比对矩阵。

2. **核验外部代码库绝对纯净**：
   Agent 产生的所有文件、分析笔记和矩阵全部安全保存在其私有的 `agent_home` 中。被观察项目的 Git 状态依然保持绝对纯净：
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
