---
title: Guides
summary: Task-oriented guides for using, operating, and integrating Holon.
order: 30
---

# Guides

Find a guide for the work you want to do: try Holon, use the Web GUI,
automate GitHub tasks, operate the runtime, or coordinate agents.

## Structure and boundaries

Guides are task-oriented (How-to) documents answering *"How do I accomplish X?"*. Each guide follows a standard structure:
1. **Goal & context:** What the guide accomplishes.
2. **Prerequisites:** Required environment, keys, or permissions.
3. **Step-by-step instructions:** Minimal reproducible commands and workflows.
4. **Verification & troubleshooting:** How to confirm success and resolve common issues.

Guides keep code snippets focused on the task. For exhaustive option dictionaries and full endpoint schemas, see [Reference](/reference/). For foundational mental models, see [Concepts](/concepts/).

Each entry below describes its workflow:

<!-- INDEX:START -->

- [Durable agent workflow](./durable-agent-workflow.md)
  The end-to-end durable agent story: create an agent, start long-running work, survive disconnects, wait for events, and deliver final briefs.
  <!-- mdorigin:index kind=article -->

- [holon solve](./solve.md)
  Use holon solve to automate GitHub issues and pull requests in headless mode.
  <!-- mdorigin:index kind=article -->

- [Local runtime](./local-runtime.md)
  A conservative workflow for running and inspecting Holon locally.
  <!-- mdorigin:index kind=article -->

- [TUI guide](./tui.md)
  Interactive terminal UI for Holon — navigation, slash commands, event log, model selection, and remote connection.
  <!-- mdorigin:index kind=article -->

- [Quick examples](./quick-examples.md)
  Common Holon tasks you can try after completing the Getting Started guide.
  <!-- mdorigin:index kind=article -->

- [Workspaces](./workspaces.md)
  Workspace lifecycle — attach, exit, detach, worktree isolation, and how workspaces differ from shell directories.
  <!-- mdorigin:index kind=article -->

- [Agent templates](./agent-templates.md)
  What agent templates are, how to sync or install them, how to use --template, and how to create custom templates.
  <!-- mdorigin:index kind=article -->

- [Documentation workflow](./documentation-workflow.md)
  How to edit and build the mdorigin-powered Holon website.
  <!-- mdorigin:index kind=article -->

- [Remote access](./remote-access.md)
  Remote daemon access — tunnel, tailnet, LAN modes, token management, and connecting from a remote TUI.
  <!-- mdorigin:index kind=article -->

- [Integration guide](./integration.md)
  Step-by-step how-to guide for integrating external systems with Holon via the HTTP control plane.
  <!-- mdorigin:index kind=article -->

- [Web GUI](./web-gui.md)
  Use Holon's embedded web interface to manage agents, monitor runtime state, and configure settings from a browser.
  <!-- mdorigin:index kind=article -->

- [Troubleshooting](./troubleshooting.md)
  Solutions for common Holon issues covering daemon, configuration, model, and TUI problems.
  <!-- mdorigin:index kind=article -->

- [Runtime observability](./observability.md)
  Export Holon traces with OTLP, scrape protected OpenMetrics, and install baseline dashboards and alerts.
  <!-- mdorigin:index kind=article -->

- [Multi-agent collaboration](./multi-agent.md)
  Creating and invoking agents, supervision contracts, and workspace modes for parallel work.
  <!-- mdorigin:index kind=article -->

- [Skills guide](./skills.md)
  Reusable SKILL.md workflows, skill locations, and how to develop custom skills.
  <!-- mdorigin:index kind=article -->

- [WebFetch and WebSearch guide](./webfetch-websearch.md)
  Agent tools for fetching web pages and searching the web — tool reference, extract modes, search providers, and usage patterns.
  <!-- mdorigin:index kind=article -->

- [Work items guide](./work-items.md)
  Durable objective tracking with work items, plans, todo lists, and lifecycle management.
  <!-- mdorigin:index kind=article -->

- [ViewImage guide](./view-image.md)
  Agent tool for inspecting local images through vision models — model selection, visual observation, durable metadata, and caching.
  <!-- mdorigin:index kind=article -->

- [Image Generation guide](./image-generation.md)
  Agent tool for generating images from text prompts — model selection, size and format options, and output management.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
