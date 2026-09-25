---
title: Guides
summary: Task-oriented how-to guides for using, operating, and integrating Holon.
order: 30
---

# Guides

Each guide answers one question: *how do I get this done?* They run from start
to finish and stay close to the task. Exact commands, flags, fields, and
endpoints live in [Reference](/reference/); the supporting mental model lives in
[Concepts](/concepts/).

The list below is grouped by the job in hand: getting a first result, running
work that waits on something, coordinating more than one agent, driving Holon
from code or a browser, working with external content, and fixing a task that
stalls.

Every guide follows the same shape: goal and context, prerequisites, steps, and
how to confirm success.

<!-- INDEX:START -->

- [Run your first Holon task](./quick-examples.md)
  Start Holon, run one task end to end, and confirm the result.
  <!-- mdorigin:index kind=article -->

- [Run a GitHub task with holon solve](./run-github-task.md)
  Take an issue or pull request from input to a finished task, then check the result.
  <!-- mdorigin:index kind=article -->

- [Run a long-lived task](./run-long-lived-task.md)
  Start work that waits, check progress, survive disconnects, and collect the final brief.
  <!-- mdorigin:index kind=article -->

- [Delegate work to another agent](./delegate-work.md)
  Hand a scoped task to a child agent, wait for the result, and handle what comes back.
  <!-- mdorigin:index kind=article -->

- [Automate Holon over HTTP](./automate-over-http.md)
  Authenticate, submit work, follow status, and read results from code.
  <!-- mdorigin:index kind=article -->

- [Connect to a remote Holon runtime](./connect-remote-runtime.md)
  Reach a runtime running on another machine and verify the connection.
  <!-- mdorigin:index kind=article -->

- [Use the Web GUI](./use-web-gui.md)
  Drive agents, work items, and skills from the browser.
  <!-- mdorigin:index kind=article -->

- [Configure OIDC authentication](./configure-oidc-authentication.md)
  Set up OpenID Connect single sign-on, configure session policies, and audit user prompts.
  <!-- mdorigin:index kind=article -->

- [Create an agent from a template](./create-agent.md)
  Pick a template, create an agent, and confirm it can take a task.
  <!-- mdorigin:index kind=article -->

- [Add a skill to an agent](./use-skills.md)
  Find a skill, install it to the library, enable it for an agent, and confirm it is active.
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

- [Troubleshoot a Holon task](./troubleshooting.md)
  Work through a stalled, failing, or silent task to a concrete next step.
  <!-- mdorigin:index kind=article -->

- [Configure advisory decisions](./configure-decision-subsystem.md)
  Set up a dedicated Decision provider and enable the AdvisoryDecision tool to give agents non-authoritative second opinions.
  <!-- mdorigin:index kind=article -->

- [Connect with the Android client](./connect-android-client.md)
  Set up the native Android client, connect to a running Holon daemon, and manage agents from mobile.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
