---
title: Guides
summary: Complete a specific task, verify the result, and diagnose common failures.
order: 30
---

# Guides

Choose a guide for the outcome you want. Each gives the prerequisites,
steps, and checks needed to finish the task. If you have not yet run a
successful request, start with [Getting started](../getting-started/).

The catalog follows a working sequence: set up agents and extensions, run
and follow continuing work, configure models and memory, connect clients,
then work with web content, images, and apps. The final guides cover checking
results, troubleshooting, and runtime telemetry.

Read [Concepts](../concepts/) for the underlying objects and boundaries.
Use [Reference](../reference/) for complete command syntax and schemas.

<!-- INDEX:START -->

- [Create an agent from a template](./create-agent.md)
  Pick a template, create an agent, and confirm it can take a task.
  <!-- mdorigin:index kind=article -->

- [Customize an agent's role and guidance](./customize-agent.md)
  Update an existing agent's standing instructions without replacing its home or reapplying a template.
  <!-- mdorigin:index kind=article -->

- [Create an agent template](./create-agent-template.md)
  Write a role contract, add optional skill dependencies, and test it with a new agent.
  <!-- mdorigin:index kind=article -->

- [Install, enable, and update skills](./use-skills.md)
  Manage the shared Skill Library and verify the skills available to a selected agent.
  <!-- mdorigin:index kind=article -->

- [Write and verify a skill](./write-skill.md)
  Author a small SKILL.md workflow, enable it for a test agent, and verify that it is loaded and followed.
  <!-- mdorigin:index kind=article -->

- [Work in a project workspace or worktree](./work-with-workspaces.md)
  Attach a project, activate its execution root, isolate changes in a managed worktree, and clean up safely.
  <!-- mdorigin:index kind=article -->

- [Run a long-lived task](./run-long-lived-task.md)
  Keep an agent working after closing your terminal, wait for dependencies, and retrieve the same task result across sessions.
  <!-- mdorigin:index kind=article -->

- [Delegate work to another agent](./delegate-work.md)
  Choose a child or peer, hand off committed revisions for review, verify results, and clean up owned children.
  <!-- mdorigin:index kind=article -->

- [Build an AI Agent ecosystem researcher for multi-repo research](./build-agent-ecosystem-researcher.md)
  Reuse the Research Steward template and multi-workspace mechanism. Learn the autonomous knowledge base design, auto-clone workspace attach, concise task prompts, and mobile link sharing workflows.
  <!-- mdorigin:index kind=article -->

- [Run a GitHub task with holon solve](./run-github-task.md)
  Scope an issue, review, or PR fix; run it once; then verify its artifacts and GitHub side effects.
  <!-- mdorigin:index kind=article -->

- [Connect external events](./connect-external-events.md)
  Connect a callback, register a wait, and verify that an external event resumes the intended work.
  <!-- mdorigin:index kind=article -->

- [Schedule agent work](./schedule-agent-work.md)
  Schedule an authorized read-only check, verify the resulting work, and cancel the timer.
  <!-- mdorigin:index kind=article -->

- [Automate Holon over HTTP](./automate-over-http.md)
  Authenticate, submit one tracked objective, follow its state, and retrieve its completion report.
  <!-- mdorigin:index kind=article -->

- [Configure models, credentials, and fallbacks](./configure-models.md)
  Authenticate a provider, select global or per-agent routes, and verify the route used for a test turn.
  <!-- mdorigin:index kind=article -->

- [Configure advisory decisions](./configure-decision-subsystem.md)
  Choose a Decision route, set call limits, and verify a recommendation or explicit abstention.
  <!-- mdorigin:index kind=article -->

- [Manage agent memory](./manage-agent-memory.md)
  Save durable preferences in the right scope and retrieve past work with traceable sources.
  <!-- mdorigin:index kind=article -->

- [Connect to a remote Holon runtime](./connect-remote-runtime.md)
  Reach a runtime from another computer or mobile phone and retrieve task results.
  <!-- mdorigin:index kind=article -->

- [Use the Web GUI](./use-web-gui.md)
  Drive agents, work items, and skills from the browser.
  <!-- mdorigin:index kind=article -->

- [Configure OIDC authentication](./configure-oidc-authentication.md)
  Set up OpenID Connect single sign-on, attribute prompts in shared team runtimes, tune session policies, and audit user activity.
  <!-- mdorigin:index kind=article -->

- [Configure OIDC Single Sign-On with Google Workspace](./google-workspace-oidc-authentication.md)
  Create an OAuth client in Google Cloud, configure Google Workspace as Holon's identity provider, enforce organization access boundaries, and verify single sign-on.
  <!-- mdorigin:index kind=article -->

- [Share AI assistants with your team](./share-with-your-team.md)
  Deploy Holon on a shared server, enable team sign-in, assign tasks to common assistants, and track progress together.
  <!-- mdorigin:index kind=article -->

- [Connect with the Android client](./connect-android-client.md)
  Set up the native Android client, pair with a running Holon daemon, and manage agents on mobile.
  <!-- mdorigin:index kind=article -->

- [Connect with the iOS client](./connect-ios-client.md)
  Set up the native iOS client, pair with a running Holon daemon, inspect deliverables and files, and report AI content on iPhone and iPad.
  <!-- mdorigin:index kind=article -->

- [Fetch and search web content](./use-web-tools.md)
  Choose between search and fetch, then read the result without trusting it blindly.
  <!-- mdorigin:index kind=article -->

- [Generate an image](./generate-image.md)
  Write a prompt, generate an image, and find the result on disk.
  <!-- mdorigin:index kind=article -->

- [Inspect an image with a vision tool](./inspect-image.md)
  Point a vision model at a local image and read back what it sees.
  <!-- mdorigin:index kind=article -->

- [Host local apps](./host-local-apps.md)
  Build a small agent-owned app, submit a request, and verify its queue events in the browser.
  <!-- mdorigin:index kind=article -->

- [Observe agent work](./observe-agent-work.md)
  Locate an agent's objective, inspect task progress, and retrieve the completion report and artifacts.
  <!-- mdorigin:index kind=article -->

- [Troubleshoot a Holon task](./troubleshooting.md)
  Identify the failing layer of a silent, stalled, or unsuccessful task without duplicating work.
  <!-- mdorigin:index kind=article -->

- [Configure observability](./configure-observability.md)
  Export traces, scrape protected metrics, and verify the telemetry path separately from task execution.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
