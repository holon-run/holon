---
title: Build an AI Agent ecosystem researcher for multi-repo research
summary: Reuse the Research Steward template and multi-workspace mechanism. Learn the autonomous knowledge base design, auto-clone workspace attach, concise task prompts, and mobile link sharing workflows.
order: 85
---

# Build an AI Agent ecosystem researcher for multi-repo research

Tracking rapidly evolving technical domains—such as AI agent frameworks, runtimes, and protocols—is difficult with one-off chat sessions or basic web searches:

- **Lack of primary source evidence**: Web summaries and second-hand blog posts quickly become outdated or imprecise. Definitive technical details live in source code architectures, commit histories, release notes, and test suites.
- **Context pollution across multiple repositories**: Technical evaluation requires comparing multiple independent open-source projects (such as different test harnesses, runtime schedulers, or protocols). Confining an agent to a single directory prevents cross-repo comparison; letting an agent write notes directly into audited codebases litters external repositories.
- **Lack of persistent knowledge accumulation**: Once a transient chat session ends, research findings and comparative matrices vanish, preventing incremental tracking and baseline reviews.

This guide demonstrates how to combine Holon's **Research Steward template** with the **Multi-Workspace Attach mechanism** to build a long-lived technical research agent named `ecosystem-researcher`.

In this system:
- **Out-of-the-box template**: Agents created from the research template come pre-equipped with necessary technical research skills; no manual skill wiring is needed.
- **Autonomous maintenance and attachment**: You do not need to clone repositories in advance. Instruct the agent to clone and attach repositories automatically. The agent maintains its private knowledge base without requiring manual directory creation or config editing.
- **Concise prompt interactions**: Code citation standards, fact-versus-inference boundaries, and report filing paths are permanently codified in the agent contract (`AGENTS.md`). Daily prompts can be brief and natural.
- **Everyday mobile workflows**: Forward open-source project links or technical documents from mobile or desktop devices. The agent performs in-depth research asynchronously in the background. Click artifact links in the brief to view the findings.

## Core mental model

Understand these core design principles before starting:

1. **Role contract and evidence discipline**:
   A research agent's primary mission is evidence gathering and objective decision briefing, not unauthorized code modification or autonomous business decisions. Reusing the `research-steward` template establishes strict evidence standards:
   - Distinguish **facts** (supported by verified commit SHAs, line numbers, or official documentation), **inferences** (architectural conclusions drawn from evidence), and **recommendations** (actionable advice for internal systems).
   - Prioritize primary sources over unverified web summaries.
   - Treat conclusions as valid specifically for the baseline commit; record updates incrementally rather than rewriting historical evaluations.

2. **Autonomous maintenance and underlying architecture**:
   You do not need to act as a file manager. The agent initializes directory structures, populates project registries, and archives comparative reports autonomously based on high-level goals and shared links. Understanding the underlying file layout explains how the agent sustains long-term memory across sessions.

3. **Agent Home decoupled from external workspaces**:
   - **`agent_home`**: The agent's private, dedicated workspace storing its long-term knowledge base, registry (`registry.yaml`), taxonomy (`taxonomy.md`), and historical deliverables.
   - **Attached external workspaces**: Multiple external open-source repositories attached into the runtime. The agent switches execution roots to inspect source code without writing research notes or temporary files into external repositories.

4. **Multi-client access and asynchronous execution**:
   Powered by Holon's background daemon and WorkItem scheduling, research runs asynchronously. You can share a GitHub link from a mobile device and lock your screen; the agent fetches, analyzes, and compares in the background, delivering a structured brief when complete.

## Before you start

- A running Holon daemon (Web GUI accessible at `http://localhost:8080`, terminal CLI `holon` available).
- **No need to pre-clone target repositories**: You can provide repository URLs directly to the agent to clone and attach, or point to existing local project directories.

## Steps

### 1. Create an agent from the research template

Holon provides the `research-steward` template designed for deep technical research with strict evidence rules.

> **Built-in skills**:
> The `research-steward` template preconfigures essential research skills upon creation (such as `ghx` for GitHub data retrieval, `sview` for structured read-only code review, and `writing-clearly-and-concisely` for disciplined briefings). **The agent is ready to work immediately; you do not need to manually configure or enable these skills.**

#### Option A: Using the Web GUI

1. **Open the Web GUI**: Navigate to Holon Web GUI in your browser (default `http://localhost:8080`).
2. **Create the Agent**:
   - Click **Templates** (`/templates`) in the left navigation menu.
   - Locate `research-steward` in the template list.
   - Click **Create Agent** and fill in the form:
     - **Agent ID**: `ecosystem-researcher`
     - **Name**: `Ecosystem Researcher`
   - Confirm creation.

#### Option B: Using the CLI

Run a single command in your terminal:

```bash
holon agent create ecosystem-researcher --template research-steward --name "Ecosystem Researcher"
```

The agent's root contract (`~/.holon/agents/ecosystem-researcher/AGENTS.md`) is loaded with the role contract and evidence guidelines.

### 2. Attach multiple open-source project workspaces

To inspect local source code across multiple repositories, attach target projects into the agent's workspace bindings.

#### Option A: Instruct the agent to clone and attach automatically (Recommended)

The simplest approach is to ask the agent directly in the conversation prompt:

> "Clone https://github.com/org/project-a and https://github.com/org/project-b locally and attach them as your workspaces."

The agent pulls the repositories into local paths and issues internal workspace attach calls automatically.

#### Option B: Attach existing local directories via CLI

If you have already cloned the target repositories locally, attach them using the CLI:

```bash
holon workspace attach --agent ecosystem-researcher /path/to/project-a
holon workspace attach --agent ecosystem-researcher /path/to/project-b
```

Verify attached workspaces:
```bash
holon agent get ecosystem-researcher
```

The `attached_workspaces` field lists all associated external paths.

> **Note**: Attaching a workspace grants read and execution authority over that directory without changing the agent's default home root. Daily operations remain centered in the agent's private `agent_home`.

### 3. Under the hood: Autonomous knowledge base architecture

> **Autonomous operation**:
> In everyday use, **you do not need to create these directories or edit YAML files manually**.
> When assigned research goals or links, the agent initializes the directory structure, updates project indices, and archives reports autonomously according to its contract.
> This section outlines the internal file structure to explain **how the agent maintains durable long-term memory**.

Inside `agent_home`, the agent automatically organizes and maintains this layout:

```text
~/.holon/agents/ecosystem-researcher/
├── AGENTS.md                  # Durable role contract and evidence guidelines
├── work/agent-ecosystem/      # Core research knowledge base (maintained by agent)
│   ├── taxonomy.md            # Ecosystem categorization (evolving taxonomy)
│   ├── registry.yaml          # Unique project registry and status tracking
│   ├── projects/              # Per-project long-term analysis archives
│   │   ├── project-a/
│   │   │   ├── metadata.yaml  # Source path, baseline commit, repository metadata
│   │   │   ├── architecture.md# Architecture breakdown and core module evidence
│   │   │   └── updates.md     # Incremental release logs (append-only)
│   │   └── project-b/
│   └── matrices/              # Cross-project comparative matrices
```

#### Project registry specification (`registry.yaml`)

When discovering a new target, the agent creates and updates entries in `registry.yaml`:

```yaml
meta:
  initialized: 2026-10-29
  description: "AI Agent ecosystem tracked project registry"

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

By storing state in structured files, the agent recovers historical context even after model context rotation or session restarts.

### 4. Launch a cross-project architecture comparison

Dispatching tasks requires no repetitive formatting or citation instructions; rules are already preserved in `AGENTS.md`.

**Example task prompt (sent directly in conversation)**:

> "Investigate the differences between project-a and project-b regarding process scheduling and interruption recovery, and create a comparative matrix."

**Autonomous agent execution loop**:
1. **Switch execution roots for code inspection**: The agent switches to `project-a` and `project-b` using internal tools, locating relevant abstractions with `sview` and recording exact line numbers and commit hashes.
2. **Archive comparative findings**: The agent switches back to `agent_home`, writes findings into `work/agent-ecosystem/matrices/task-resumption.md`, and updates per-project `architecture.md` files.
3. **Deliver a concise brief**: The agent returns a structured summary in the conversation with links to generated artifacts.

### 5. Inspect research deliverables and verify isolation

1. **Review findings**:
   Once finished, the agent outputs an executive summary in the chat.
   - Click the **deliverable links** inside the completion brief, or open `work/agent-ecosystem/matrices/task-resumption.md` directly in the Web GUI left file explorer to review the complete matrix.

2. **Verify external repository purity**:
   All notes, logs, and matrices reside strictly in `agent_home`. Verify that external repositories remain untouched:
   ```bash
   git -C /home/user/src/github.com/org/project-a status
   git -C /home/user/src/github.com/org/project-b status
   ```
   Both output `working tree clean`. Research activity produces zero file pollution in observed repositories.

---

## Everyday workflow: Link sharing and mobile asynchronous research

Technical discovery frequently happens outside desktop coding sessions—while reading technical news, social feeds, or community posts on mobile devices.

### Mobile link sharing

When discovering an interesting open-source repository or architecture document on your phone (via the [iOS client](./connect-ios-client.md), [Android client](./connect-android-client.md), or mobile browser):

1. **Share or send the link to the Agent**:
   Paste the link into the Holon conversation with a concise note:
   > "Research this new runtime: https://github.com/example/novel-runtime . Focus on its scheduling model and how it compares to our observed projects."

2. **Asynchronous background execution**:
   - Lock your screen or switch apps immediately. Holon runs as a persistent daemon with durable WorkItems; active client connections are not required.
   - The agent fetches the repository README, releases, and documentation using web and GitHub tools (`WebFetch`, `WebSearch`, `ghx`).
   - It records the entry in `registry.yaml` and produces an initial evaluation in `projects/novel-runtime/architecture.md`.

3. **Review deliverables anywhere**:
   - Reopen your phone client or desktop Web GUI later. The evaluation is ready in your conversation feed, outlining positioning, architectural advantages, limitations, and ecosystem fit.
   - If the project warrants deep dive inspection, clone the repository locally upon returning to your workstation and attach it as a workspace. The agent continues from existing findings directly into AST-level code inspection.

---

## Advanced: Incremental tracking and scheduled patrols

As open-source projects evolve, avoid repeating full-scale research on each release:

- **Baseline commit locking**: Keep evaluations pinned to `baseline_commit` recorded in `registry.yaml`. Compare changes only when intentionally bumping baselines.
- **Configure scheduled patrols**:
  Use Holon's timer mechanism (see [Schedule agent work](./schedule-agent-work.md)) to configure weekly or daily automated wakeups in the Web GUI or CLI. The agent checks for upstream releases or commits, recording breaking changes into `updates.md` to maintain an active technical radar.

## Related reading

- [Use the Web GUI](./use-web-gui.md): Browser-based agent management, template setup, and artifact inspection.
- [Connect the iOS client](./connect-ios-client.md) / [Connect the Android client](./connect-android-client.md): Interact with agents and inspect deliverables on mobile devices.
- [Work in a project workspace or worktree](./work-with-workspaces.md): Workspace attach, execution root switching, and worktree isolation.
- [Create an agent from a template](./create-agent.md): Overview of template initialization and standard roles.
- [Schedule agent work](./schedule-agent-work.md): Configure one-time or recurring timers for ongoing background monitoring.
- [Fetch and search web content](./use-web-tools.md): Use web search and page fetching tools for primary technical research.
