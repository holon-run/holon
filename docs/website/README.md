---
title: Holon
summary: Agents follow through without constant nudging. Holon is a local workbench for ongoing agent work. You set the goals and boundaries; agents save progress, wait for changes, and pick up where they left off.
order: 1
---

<div class="home-page home-page--en">
<section class="home-hero">
<div class="home-hero__copy">

<p class="home-eyebrow">From a single conversation to ongoing collaboration</p>

# Agents follow through.<br>Without constant nudging.

<p class="home-hero__lede">Give agents work that needs ongoing attention. You set the goals and boundaries; they follow through as agreed, act when things change, and ask for your judgment or approval when needed.</p>

<div class="home-actions">
<a class="home-button home-button--primary" href="#install">Get started</a>
<a class="home-button home-button--quiet" href="#capabilities">See how it works ↓</a>
</div>

<p class="home-hero__note">Holon, a local workbench for ongoing agent work.</p>

</div>
<div class="home-runtime" aria-label="You set the goals and boundaries. Agents work, save progress, wait for changes, and resume. They ask for your judgment or approval when needed and deliver results when the task is complete.">
<div class="home-runtime__top"><span>You + agents · ongoing collaboration</span><span class="home-runtime__status">illustration</span></div>
<div class="home-followup__entry"><strong>You set the goals and boundaries</strong><span>Agree on permissions · Define the outcome</span></div>
<div class="home-followup">
<div class="home-followup__cycle" role="group" aria-label="Clockwise cycle: do the work, save progress, wait for changes, resume work">
<div class="home-followup__step home-followup__step--work"><strong>Do the work</strong><small>Real tools and workspaces</small></div>
<span class="home-followup__arrow home-followup__arrow--top" aria-hidden="true">→</span>
<div class="home-followup__step home-followup__step--record"><strong>Save progress</strong><small>State and next steps</small></div>
<span class="home-followup__arrow home-followup__arrow--right" aria-hidden="true">↓</span>
<div class="home-followup__center"><strong>Agents follow through</strong><span>Continue the same work</span></div>
<div class="home-followup__step home-followup__step--wait"><strong>Wait for changes</strong><small>Results or agreed conditions</small></div>
<span class="home-followup__arrow home-followup__arrow--bottom" aria-hidden="true">←</span>
<div class="home-followup__step home-followup__step--resume"><strong>Resume work</strong><small>When conditions are met</small></div>
<span class="home-followup__arrow home-followup__arrow--left" aria-hidden="true">↑</span>
</div>
<aside class="home-followup__human"><span aria-hidden="true">⇄</span><strong>You step in</strong><p>For decisions<br>or approval</p><small>Then work<br>continues</small></aside>
</div>
<p class="home-followup__note">Pause while waiting. Resume when things change.<br><span>Deliver results when the task is complete.</span></p>
</div>
</section>

<section class="home-section home-capability-section" id="capabilities">
<div class="home-section__intro">
<p class="home-kicker">How Holon works</p>

## One core work model, supported by three runtime capabilities.

Organize work around explicit goals and progress. Long-lived agents carry it forward, resume when waiting conditions are met, and delegate tasks when work needs to be split.

</div>
<div class="home-capability-system">
<article class="home-capability-main">
<div class="home-capability-main__heading"><span>01</span><small>WorkItem</small></div>
<h3>Give goals, progress, and waits a place to belong.</h3>
<p>Work that needs ongoing attention should not live only in chat history. A WorkItem preserves its goal, plan, progress, and waiting conditions. After a pause, the agent can pick up the same work where it left off.</p>
<div class="home-state-sequence" aria-label="Ongoing collaboration"><span>Set a goal</span><i>→</i><span>Save progress</span><i>→</i><span>Wait</span><i>→</i><span>Resume</span><i>→</i><span>Deliver</span></div>
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
<p class="home-kicker">The workbench behind ongoing collaboration</p>

## No need to watch the chat. Your work has a home.

Holon is not another agent. It is a local runtime that lets agents keep working. Running as a background service, it saves work state and manages waits and wakeups. Return through the TUI or Web UI to check progress, add information, or change direction.

</div>
<picture class="home-architecture">
<source media="(max-width: 1100px)" srcset="/assets/runtime-architecture-en-narrow.png" width="480" height="550">
<img src="/assets/runtime-architecture-en.png" width="1200" height="350" alt="TUI and Web UI connect to one persistent Holon service; Mobile has a dashed outline and connection. Holon supports ongoing work, saved state and event wakeups, managing agents and work items with workspaces, files and toolchains on its host." loading="lazy" decoding="async">
</picture>
<p class="home-product__caption">Execution requires the daemon and host to stay running. The machine boundary indicates where work runs; it does not provide additional permission isolation.</p>
</section>

<section class="home-section home-proof">
<div class="home-proof__intro">
<p class="home-kicker">Featured articles</p>

## From understanding Holon to putting it to work.

Explore the design, set up continuous PR review, and see how people and agents work together.

</div>
<a class="home-reading-all" href="/blog/">View all articles →</a>
<div class="home-reading-grid">
<a class="home-reading-card" href="/blog/what-is-holon">
<img class="home-reading-card__cover" src="/assets/holon-agent-workspace-cover.webp" width="1672" height="941" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">01 · PRODUCT</span>
<h3>Let multiple agents keep working in your environment</h3>
<p>Move from asking AI for help each time to giving agents ongoing roles. Meet the local workbench that supports this way of working.</p>
<span class="home-reading-card__cta">Meet Holon →</span>
</a>
<a class="home-reading-card" href="/blog/why-work-items">
<img class="home-reading-card__cover" src="/assets/workitem-abstract-flow-cover.webp" width="1536" height="768" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">02 · DESIGN</span>
<h3>WorkItem: separate work state from the conversation</h3>
<p>From an API migration to alternating between two PRs, see how goals, progress, and waiting conditions survive across turns.</p>
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
<h3>Everyone has AI. How does the team work together?</h3>
<p>From device failures to testing silent piano keys, see how shared agents fit into a team's workflow and follow up on people's test results.</p>
<span class="home-reading-card__cta">Read the team story →</span>
</a>
</div>
</section>

<section class="home-section home-start-section" id="install">
<div class="home-start">
<div class="home-start__copy">
<p class="home-kicker">Start locally</p>

## Get ready for your first ongoing task.

Install Holon, configure a model provider, and start the daemon. Open the local Web interface at `http://localhost:7878` or use `holon tui`.

<p class="home-start__boundary">Recommended release: <a href="https://github.com/holon-run/holon/releases/tag/v0.40.0">v0.40.0</a></p>

<div class="home-actions">
<a class="home-button home-button--primary" href="/getting-started/">Full setup guide</a>
<a class="home-button home-button--quiet" href="https://github.com/holon-run/holon/releases">Download binaries ↗</a>
</div>

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

## Start with one task you don't want to keep checking on.

Set a clear goal. Agree on how to follow up, what to deliver, and when you need to be involved. Spend less time checking and prompting, and more time on what matters to you.

</div>
<div class="home-final__actions">
<a class="home-button" href="/getting-started/first-agent">Create your first agent</a>
<a href="/blog/">Read the practical guides →</a>
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

<!-- INDEX:END -->
