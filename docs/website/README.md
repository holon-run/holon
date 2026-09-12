---
title: Holon
summary: Let agents keep working in your environment. Holon preserves progress and resumes work when there is something to act on.
order: 1
---

<div class="home-page">
<section class="home-hero">
<div class="home-hero__copy">

<p class="home-eyebrow">Local workbench for long-lived agents</p>

# Let agents keep working.<br>Connect when you need to.

<p class="home-hero__lede">Give recurring reviews, investigations, and tests to agents with ongoing roles. Holon runs them in your repositories and toolchains, saves their progress, and resumes work when a commit, check result, or your approval arrives.</p>

<div class="home-actions">
<a class="home-button home-button--primary" href="/getting-started/">Get started</a>
<a class="home-button home-button--quiet" href="https://github.com/holon-run/holon">View on GitHub ↗</a>
</div>

<p class="home-hero__note">A local-first runtime around agents—not another agent or a managed agent service.</p>

</div>
<div class="home-runtime" aria-label="Illustrated Holon runtime workflow">
<div class="home-runtime__top"><span>Holon Runtime · workflow</span><span class="home-runtime__status">illustration</span></div>
<div class="home-runtime__events"><span>operator input</span><span>task result</span><span>external event</span><span>timer</span></div>
<div class="home-runtime-map">
<div class="home-runtime-map__stage home-runtime-map__stage--core"><span class="home-runtime-map__label">LONG-LIVED AGENT</span><strong>Ongoing roles, lasting responsibility</strong><small>Review, investigate, test—with goals and progress saved for each piece of work.</small></div>
<div class="home-runtime-map__connector" aria-hidden="true"><span>execute</span></div>
<div class="home-runtime-map__pair">
<div class="home-runtime-map__stage"><span class="home-runtime-map__label">WORKSPACE</span><strong>Repositories + tools</strong><small>Work happens in real project environments.</small></div>
<div class="home-runtime-map__stage"><span class="home-runtime-map__label">CONTROL</span><strong>Trust + approval</strong><small>Boundaries remain explicit.</small></div>
</div>
<div class="home-runtime-map__connector" aria-hidden="true"><span>wait · wake · resume</span></div>
<div class="home-runtime-map__stage home-runtime-map__stage--output"><span class="home-runtime-map__label">DELIVERY</span><strong>Operator-facing brief</strong><small>Results stay separate from execution traces.</small></div>
</div>
</div>
</section>

<section class="home-section home-capability-section" id="capabilities">
<div class="home-section__intro">
<p class="home-kicker">How Holon works</p>

## One operating model, supported by three runtime capabilities.

Organize work around explicit goals and progress. Long-lived agents carry it forward, resume when waiting conditions are met, and delegate tasks when work needs to be split.

</div>
<div class="home-capability-system">
<article class="home-capability-main">
<div class="home-capability-main__heading"><span>01</span><small>WorkItem</small></div>
<h3>Give goals, progress, and waits a place to belong.</h3>
<p>In Holon, a WorkItem holds an ongoing objective, its plan, progress, waiting conditions, and completion brief. After a pause, the agent continues the same WorkItem instead of reconstructing the work from chat history.</p>
<div class="home-state-sequence" aria-label="WorkItem lifecycle"><span>event</span><i>→</i><span>WorkItem</span><i>→</i><span>wait</span><i>→</i><span>wake</span><i>→</i><span>brief</span></div>
<footer>WorkItem · objective · plan · progress · waits · completion brief</footer>
</article>
<div class="home-capability-support">
<article class="home-capability-row"><span>02</span><div><small>EVENT-DRIVEN CONTINUATION</small><h3>Wait for a reason. Resume on a signal.</h3><p>Record what a WorkItem is waiting for: a task result, an external event, a timer, or your input. When that condition is met, continue the corresponding WorkItem.</p></div></article>
<article class="home-capability-row"><span>03</span><div><small>LONG-LIVED AGENT IDENTITY</small><h3>Keep the owner, not just the conversation.</h3><p>Give each agent an ongoing role, its own instructions, and durable memory. Return to the same agent across sessions to continue the work it owns.</p></div></article>
<article class="home-capability-row"><span>04</span><div><small>MULTI-AGENT COLLABORATION</small><h3>Delegate work. Bring results back.</h3><p>Hand tasks to existing agents or create subagents to work in parallel. Track progress, wait for results, then continue the main work.</p></div></article>
</div>
</div>
</section>

<section class="home-section home-product">
<div class="home-section__intro home-section__intro--narrow">
<p class="home-kicker">One runtime · multiple interfaces</p>

## Close the interface. Keep the work in place.

In daemon mode, Holon Runtime holds agents, work items, and waiting state. TUI and Web UI connect to the same runtime; closing an interface does not stop the background service.

</div>
<picture class="home-architecture">
<source media="(max-width: 1100px)" srcset="/assets/runtime-architecture-en-narrow.png" width="480" height="550">
<img src="/assets/runtime-architecture-en.png" width="1200" height="350" alt="TUI and Web UI connect to one persistent Holon service; Mobile has a dashed outline and connection. Holon supports ongoing work, saved state and event wakeups, managing agents and work items with workspaces, files and toolchains on its host." loading="lazy" decoding="async">
</picture>
<p class="home-product__caption">Execution requires the daemon and host to stay running. The machine boundary shows where work runs, not additional permission isolation.</p>
</section>

<section class="home-section home-proof">
<div class="home-proof__intro">
<p class="home-kicker">From the blog</p>

## Meet Holon. Then see how the work continues.

Start with the product and its WorkItem design, set up continuous PR review, then see how shared agents fit into a small team's day-to-day work.

</div>
<a class="home-reading-all" href="/blog/">View all articles →</a>
<div class="home-reading-grid">
<a class="home-reading-card" href="/blog/what-is-holon">
<img class="home-reading-card__cover" src="/assets/holon-agent-workspace-cover.webp" width="1672" height="941" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">01 · PRODUCT</span>
<h3>Let agents keep working in your environment</h3>
<p>Give agents ongoing roles and connect when you need to. Meet the local workbench, from your own machine to remote development and team use.</p>
<span class="home-reading-card__cta">Meet Holon →</span>
</a>
<a class="home-reading-card" href="/blog/why-work-items">
<img class="home-reading-card__cover" src="/assets/workitem-abstract-flow-cover.webp" width="1536" height="768" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">02 · DESIGN</span>
<h3>WorkItems: work state beyond the conversation</h3>
<p>From an API migration to two PRs moving in parallel, see how goals, progress, and waiting conditions survive across turns.</p>
<span class="home-reading-card__cta">Explore the design →</span>
</a>
<a class="home-reading-card" href="/blog/one-pr-one-work-item">
<img class="home-reading-card__cover" src="/assets/continuous-pr-reviewer-cover.webp" width="1672" height="941" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">03 · REVIEWER</span>
<h3>Build a reviewer that follows PRs through</h3>
<p>Start with the code-reviewer template, agree on responsibilities and permissions, then subscribe to PRs and follow fixes, CI, and merges.</p>
<span class="home-reading-card__cta">Set up continuous review →</span>
</a>
<a class="home-reading-card" href="/blog/agents-in-a-small-team">
<img class="home-reading-card__cover" src="/assets/team-shared-agents-cover.webp" width="1536" height="1024" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">04 · TEAM</span>
<h3>From personal AI tools to shared team agents</h3>
<p>Device investigations and hands-on testing show how shared agents join a team's workflow and pick up where people leave off.</p>
<span class="home-reading-card__cta">Read the team story →</span>
</a>
</div>
</section>

<section class="home-section home-start-section">
<div class="home-start">
<div class="home-start__copy">
<p class="home-kicker">Local-first by design</p>

## Start with one useful responsibility.

Install Holon, configure a model provider, and start the daemon. Open the local Web interface at `http://localhost:7878` or use `holon tui`.

<p class="home-start__boundary">Early-stage software · explicit trust boundaries · approval remains visible</p>

<div class="home-actions"><a class="home-button home-button--primary" href="/getting-started/">Follow the complete setup</a></div>

</div>

```bash
brew tap holon-run/tap && brew install holon
holon onboard
holon daemon start
```

</div>
</section>

<section class="home-final">
<div>
<p class="home-kicker">Continue from here</p>

## Give project work a durable home.

Start locally, keep the first workflow bounded, and let Holon preserve the work when the chat or terminal ends.

</div>
<div class="home-final__actions">
<a class="home-button" href="/getting-started/">Get started</a>
<a href="/blog/">Read the field notes →</a>
</div>
</section>
</div>

<!-- INDEX:START -->

- [Runtime specs](./spec/)
  <!-- mdorigin:index kind=directory -->

- [Blog](./blog/)
  <!-- mdorigin:index kind=directory -->

- [Getting started](./getting-started/)
  <!-- mdorigin:index kind=directory -->

- [Concepts](./concepts/)
  <!-- mdorigin:index kind=directory -->

- [Guides](./guides/)
  <!-- mdorigin:index kind=directory -->

- [Reference](./reference/)
  <!-- mdorigin:index kind=directory -->

- [dist](./dist/)
  <!-- mdorigin:index kind=directory -->

<!-- INDEX:END -->
