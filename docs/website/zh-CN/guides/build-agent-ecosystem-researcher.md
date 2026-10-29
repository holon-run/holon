---
title: 搭建 AI Agent 生态研究员：跨多开源项目的持续深度调研
summary: 复用 Research Steward 模板与多 Workspace 挂载机制，打造一个持续跟踪开源生态、保持代码库纯净并沉淀证据资产库的研究 Agent。
order: 85
---

# 搭建 AI Agent 生态研究员：跨多开源项目的持续深度调研

在跟踪快速演进的技术领域（例如 AI Agent 框架、运行时与协议）时，单纯依赖一次性的模型对话或网页搜索往往难以胜任：

- **缺乏一手源码证据**：网页抓取与二次解读容易泛化甚至过时，真正的关键细节隐藏在开源项目的架构实现、提交历史、Release 契约和测试用例中；
- **多项目上下文混乱**：技术调研往往需要横向对比多个独立的开源项目（例如不同的 Harness、运行时调度器或通信协议）。如果将 Agent 限制在单个目录，它无法跨仓库对比；如果让 Agent 在被测仓库内随意记录笔记，又会污染原始代码库；
- **缺乏长期资产沉淀**：一次会话结束，研究过程和对比矩阵便丢失了，无法做增量跟踪和基线复核。

本指南演示如何利用 Holon 的 **Research Steward（研究管家）模板** 理念以及 **多工作区挂载（Multi-Workspace Attach）与隔离机制**，搭建一个名为 `ecosystem-researcher` 的长效技术调研 Agent。该 Agent 既能深度只读审查多个开源项目的真实代码，又能在自己独立的 Home 空间中沉淀分类法、项目索引表和研究报告。

## 核心心智模型

在动手前，先理解本场景涉及的两个核心设计边界：

1. **研究角色契约（Role Contract）与证据准则**：
   研究类 Agent 的核心职责是“证据收集与决策简报”，而不是“越权替人类做决策”或“修改产品代码”。复用 `research-steward` 模板可以为 Agent 注入严格的证据规则：
   - 区分**事实**（有明确源码 commit/行号或官方文档支撑）、**判断**（基于多方证据的架构推论）与**建议**（对内部产品的行动意见）；
   - 一手来源优先，不将未经引证的搜索摘要直接作为结论；
   - 结论默认只对基线 Commit 有效，增量更新采用追加记录，不随意推倒重来。

2. **Agent 专属 Home 与外部代码工作区解耦**：
   - **`agent_home`**：Agent 的独立私有空间，用于保存该 Agent 的长期知识库、调研登记表（`registry.yaml`）、生态分类法（`taxonomy.md`）和历史产物；
   - **挂载外部 Workspace**：通过 `holon workspace attach` 将多个开源代码仓库作为外部只读工作区挂载进来。Agent 可以通过 `SwitchWorkspace` 在各个项目之间平滑切换执行根进行代码审查，而不会把任何研究过程产物写入被测仓库。

## 前置条件

- 本地已安装并启动 Holon 守护进程（终端中 `holon` CLI 可用）。
- 本地磁盘已克隆或准备好若干需要被观察的开源项目仓库（例如本文示例中的两个项目）：
  - `/home/user/src/github.com/org/project-a`
  - `/home/user/src/github.com/org/project-b`
- 具备基础的 Git 与终端操作经验。

## 操作步骤

### 1. 从研究模板创建 Agent 并配置专业 Skill

Holon 提供了面向深度调研场景的 Agent 模板 `research-steward`。该模板预置了严谨的研究证据收集契约。

1. **创建研究 Agent 实例**：
   在终端中运行以下命令，从 `research-steward` 模板初始化一个名为 `ecosystem-researcher` 的长效 Agent：

   ```bash
   holon agent create ecosystem-researcher --template research-steward --name "生态调研员"
   ```

2. **按需启用调研必备的 Skill**：
   研究 Agent 通常需要以下能力支撑（安装到全局 Library 并为该 Agent 启用）：
   - `ghx`：用于精准读取 GitHub 仓库的 Release、Issue、PR 与提交历史；
   - `sview`：提供结构化、只读的文件审查与切片浏览能力；
   - `citation-verifier`：核验引文真实性与来源对应关系；
   - `writing-clearly-and-concisely`：保证研究简报风格清晰、精炼，避免大模型套话。

   ```bash
   holon skills enable ghx --agent ecosystem-researcher
   holon skills enable sview --agent ecosystem-researcher
   holon skills enable citation-verifier --agent ecosystem-researcher
   holon skills enable writing-clearly-and-concisely --agent ecosystem-researcher
   ```

3. **核对长效角色契约（`AGENTS.md`）**：
   Agent 根目录下的 `AGENTS.md`（位于 `~/.holon/agents/ecosystem-researcher/AGENTS.md`）定义了其长效行为准则。你可以直接查看并根据需要补充局部约定，例如：
   - 源码 Workspace 默认只读，禁止修改外部项目代码；
   - 关键架构结论必须精确给出 `repo + commit + path + symbol/line range`；
   - 所有产物均统一输出到本 Agent 的 `work/` 目录。

### 2. 挂载多个开源项目工作区（Multi-Workspace Attach）

这是让 Agent 能够“横向观察多个项目”的核心机制。

1. **将多个外部开源项目挂载至该 Agent**：

   ```bash
   # 挂载项目 A
   holon workspace attach --agent ecosystem-researcher /home/user/src/github.com/org/project-a

   # 挂载项目 B
   holon workspace attach --agent ecosystem-researcher /home/user/src/github.com/org/project-b
   ```

2. **验证 Agent 的工作区挂载状态**：
   运行 `holon agent get` 查看该 Agent 的配置信息：

   ```bash
   holon agent get ecosystem-researcher
   ```

   在输出的 `attached_workspaces` 列表中，可以看到当前 Agent 已经同时关联了自身默认的 `agent_home` 以及刚刚挂载的多个外部代码仓库。

> **注意**：`workspace attach` 仅将工作区权限赋予该 Agent，并不会自动切换当前的执行根。此时 Agent 的活动工作区依然是其私有的 `agent_home`。

### 3. 在 Agent Home 中建立结构化的研究知识库

在 Agent 专属的 `agent_home` 内建立目录体系，用于长效沉淀资产。典型的研究知识库布局如下：

```text
~/.holon/agents/ecosystem-researcher/
├── AGENTS.md                  # 长效角色契约与证据准则
├── work/agent-ecosystem/      # 核心研究知识库
│   ├── taxonomy.md            # 生态分类法与评估维度
│   ├── registry.yaml          # 被观察对象的唯一登记索引表
│   ├── projects/              # 各项目的独立长期分析
│   │   ├── project-a/
│   │   │   ├── metadata.yaml  # 源码路径、基线 commit、开源协议
│   │   │   ├── architecture.md# 架构拆解与核心抽象
│   │   │   └── updates.md     # 随版本发布的增量跟踪记录
│   │   └── project-b/
│   └── matrices/              # 跨项目横向比对矩阵
```

在 `work/agent-ecosystem/registry.yaml` 中登记每个被观察对象的基线状态：

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

### 4. 发起一次跨项目的深度架构对比任务

现在，你可以向 Agent 下达具体的横向调研指令。Agent 会在被挂载的外部项目与私有知识库之间协同工作：

**任务指令示例**：

> “请对已挂载的 project-a 与 project-b 进行一次横向技术调研：
> 1. 分别审查两者在 Agent 进程调度与上下文中断恢复机制上的实现；
> 2. 给出关键代码位置（包括 commit 与行号范围）；
> 3. 比较两者在长任务持久化设计上的优缺点；
> 4. 将对比结果沉淀到 `work/agent-ecosystem/matrices/task-resumption.md`，并更新两项目的 `architecture.md`。”

**Agent 的底层执行逻辑**：
1. **审查 Project A**：Agent 调用 `SwitchWorkspace` 将执行根切换至 `project-a`，利用只读工具（如 `sview` 或 `rg`）审查调度模块，记录具体文件名与行号证据；
2. **审查 Project B**：Agent 再次调用 `SwitchWorkspace` 将执行根切换至 `project-b`，提取对应的状态机实现并完成交叉核对；
3. **沉淀产物回 Home**：Agent 切换回自己的 `agent_home`（或以绝对路径写入自身知识库），生成分析矩阵，而外部的 `project-a` 和 `project-b` 代码目录完全不受污染。

### 5. 验证调研产物与隔离效果

1. **检查调研成果**：
   查看 Agent 生成的研究矩阵：
   ```bash
   cat ~/.holon/agents/ecosystem-researcher/work/agent-ecosystem/matrices/task-resumption.md
   ```
   确认报告中是否满足证据准则：包含明确的结论对比表、源码引证精确到 Commit 和行号、区分客观事实与主观推论。

2. **验证外部被测项目保持纯净**：
   进入被测开源项目目录检查 Git 状态：
   ```bash
   git -C /home/user/src/github.com/org/project-a status
   git -C /home/user/src/github.com/org/project-b status
   ```
   输出应为 `working tree clean`，被测仓库完全未受修改。

## 进阶技巧：长期增量跟踪与定时巡检

当生态中的开源项目持续发布新版本时，无需每次重新进行全量调研：
- **基线 Commit 锁定**：始终以 `registry.yaml` 中记录的 `baseline_commit` 为基准。只有当决定升级基线时才执行 rebase 或更新。
- **结合定时器定期巡检**：利用 Holon 的定时器机制（参阅[调度 Agent 工作](./schedule-agent-work.md)），定期唤醒 Agent 检查上游仓库的 Git Log 或 GitHub Release 变更。只对产生重大影响的更新生成增量记录（`updates.md`），持续维护活态生态知识库。

## 相关内容

- [在项目工作区或 worktree 中工作](./work-with-workspaces.md)：工作区挂载、执行根切换与 Worktree 隔离机制。
- [从模板创建 Agent](./create-agent.md)：了解 Agent 模板初始化流程与内置模板。
- [调度 Agent 工作](./schedule-agent-work.md)：配置单次或周期性定时器执行长期巡检。
- [获取与搜索网络内容](./use-web-tools.md)：结合网络搜索与文档抓取补全开源项目的一手资料。
