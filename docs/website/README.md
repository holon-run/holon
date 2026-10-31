---
title: Create and run AI assistants
summary: Create and run AI assistants on your own computer or server. Assign tasks, track progress, and let them follow up. Open source for individuals and teams.
order: 1
---

<div class="home-page home-page--en">
<section class="home-hero">
<div class="home-hero__copy">

<p class="home-eyebrow">Built for work that needs follow-through</p>

# Create and run AI assistants on your own computer or server.

<p class="home-hero__lede">Open-source software for individuals and teams. Assign tasks to your assistants, track their progress, and let them follow up on ongoing work.</p>

<div class="home-actions">
<a class="home-button home-button--primary" href="#install">Install and start</a>
<a class="home-button home-button--quiet" href="#capabilities">See how it works ↓</a>
</div>

<p class="home-hero__note">Give each assistant a role and a task. Check progress, provide input when needed, and review the result.</p>

<div class="home-litepaper" role="group" aria-label="Holon product overview PDF">
<span class="home-litepaper__label">Product overview</span>
<a href="/assets/lite-paper/holon-lite-paper-en.pdf" type="application/pdf" hreflang="en" aria-label="Read the Holon lite paper in English (PDF)">Read <span aria-hidden="true">↗</span></a>
</div>

</div>
<div class="home-runtime" aria-label="Within the roles and permissions you define, tasks, system events, and schedules trigger agents to start or resume work. Agents save progress, wait for results, and ask for decisions or approval when needed.">
<div class="home-runtime__top"><span>Act on events · Keep track of progress</span><span class="home-runtime__status">illustration</span></div>
<div class="home-followup__entry"><strong>You define the role, permissions, and outcome</strong><span>Start or resume work with these inputs ↓</span></div>
<div class="home-trigger-sources" role="group" aria-label="Ways to trigger work"><div><strong>Your input</strong><span>Tasks · Feedback</span></div><div><strong>System events</strong><span>Email · Orders · CI</span></div><div><strong>Schedules</strong><span>Checks · Reports</span></div></div>
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
<p class="home-followup__note">Start or resume when an event arrives or a task is due.<br><span>Deliver results when the task is complete.</span></p>
</div>
</section>

<section class="home-section home-start-section" id="install">
<div class="home-start">
<div class="home-start__copy">
<p class="home-kicker">Quick start</p>

## Install. Configure. Put an assistant to work.

Install Holon, then run `holon onboard` to configure a model provider and `holon daemon start` to start the background service.

Open `http://localhost:7878` or run `holon tui` to create your first assistant and assign a task.

<p class="home-start__boundary">Recommended release: <a href="https://github.com/holon-run/holon/releases/tag/v0.56.0">v0.56.0</a></p>

<div class="home-actions">
<a class="home-button home-button--primary" href="/getting-started/">Setup guide · all platforms</a>
<a class="home-button home-button--quiet" href="https://github.com/holon-run/holon/releases">Download binaries ↗</a>
</div>

</div>

<div class="home-start__commands">
<p class="home-start__command-label">macOS · Homebrew</p>

```bash
brew tap holon-run/tap && brew install holon
holon onboard
holon daemon start
```

</div>

</div>
</section>

<section class="home-section home-capability-section" id="capabilities">
<div class="home-section__intro">
<p class="home-kicker">How Holon works</p>

## Do the work. Wait for results. Pick up where you left off.

Some work takes several rounds of action and waiting. Holon saves progress so agents can continue when new information arrives—and you can see where things stand.

</div>
<div class="home-capability-system">
<article class="home-capability-main">
<div class="home-capability-main__heading"><span>01</span><small>WorkItem</small></div>
<h3>Save progress. Resume the same task.</h3>
<p>A work item (WorkItem) keeps the goal, plan, progress, and pending results together. Whether an agent is waiting for an order update or a teammate’s test results, it can use the saved progress to continue when the update arrives.</p>
<div class="home-state-sequence" aria-label="Steps in an ongoing task"><span>Set a goal</span><i>→</i><span>Save progress</span><i>→</i><span>Wait</span><i>→</i><span>Resume</span><i>→</i><span>Deliver</span></div>
<footer>WorkItem · goal · plan · progress · pending results · final summary</footer>
</article>
<div class="home-capability-support">
<article class="home-capability-row"><span>02</span><div><small>EVENTS AND SCHEDULES</small><h3>Respond to updates. Act on schedule.</h3><p>Connect external systems through webhooks or set up scheduled tasks. Agents can start work when triggered, then wait for results or feedback before continuing.</p></div></article>
<article class="home-capability-row"><span>03</span><div><small>ROLES AND MEMORY</small><h3>Give each agent a lasting role.</h3><p>Define what each agent handles, with its own skills, instructions, and memory. Return to the same agent to continue the work you assigned it.</p></div></article>
<article class="home-capability-row"><span>04</span><div><small>TASK DELEGATION</small><h3>Split up tasks. Bring results together.</h3><p>Assign investigation, review, and testing to different agents, or create subagents to work in parallel. Track their tasks and use the results to move the overall job forward.</p></div></article>
</div>
</div>
</section>

<section class="home-section home-product">
<div class="home-section__intro home-section__intro--narrow">
<p class="home-kicker">For you and your team</p>

## Check progress. Keep your team’s work moving.

Use the web or terminal interface (TUI) to check tasks, add information, and change direction. On a team server, teammates can assign tasks to the same agent, share results, and continue from the saved progress.

Keep project files and outputs in separate workspaces. Agents work in the background while you and your team check in as needed.

</div>
<figure class="home-workbench-preview">
<a href="/assets/lite-paper/web-gui-review-en.png" aria-label="View the full-size web workbench screenshot">
<img src="/assets/lite-paper/web-gui-review-en.png" width="3000" height="1880" alt="Holon web workbench: agents organized by role on the left, review progress and next steps in the center, and the current task, pending results, and checklist on the right." loading="lazy" decoding="async">
</a>
<figcaption>See what’s done, what’s pending, and what happens next. Screenshot uses demo data. Click to enlarge.</figcaption>
</figure>
<p class="home-product__caption">Ongoing work requires the computer or server and the Holon background service to stay running. Your configuration determines which files and tools agents can use.</p>
<div class="home-mobile">
<h3>Mobile clients · Beta</h3>
<p>Connect your phone to Holon running on your computer or server. Android testing instructions require Google Group access. You can also install the APK directly.</p>
<div class="home-actions">
<a class="home-button home-button--quiet" href="https://testflight.apple.com/join/XkTfN12S">iOS · TestFlight ↗</a>
<a class="home-button home-button--quiet" href="https://groups.google.com/g/holon-pioneers/c/Cgs_KEAjdN0">Android · Join testing ↗</a>
<a class="home-button home-button--quiet" href="https://github.com/holon-run/holon/releases/download/v0.56.0/holon-android-v0.56.0.apk">Download Android APK ↗</a>
</div>
</div>
</section>

<!-- TEMPLATES:START -->
<!-- Template cards are generated from agent_templates and template-display.json. -->
<!-- TEMPLATES:END -->

<section class="home-section home-proof">
<div class="home-proof__intro">
<p class="home-kicker">Featured articles</p>

## See what you can do with Holon.

Start with a hands-on task, see how a team shares an assistant, and get to know Holon.

</div>
<div class="home-reading-grid">
<a class="home-reading-card" href="/blog/from-prompt-to-finished-work">
<img class="home-reading-card__cover" src="/assets/holon-agent-workspace-cover.webp" width="1672" height="941" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">01 · WORKFLOW</span>
<h3>From prompt to finished work</h3>
<p>Give an agent a bounded repository task, let it work past the prompt, and return to a result you can inspect.</p>
<span class="home-reading-card__cta">Follow the workflow →</span>
</a>
<a class="home-reading-card" href="/blog/agents-in-a-small-team">
<img class="home-reading-card__cover" src="/assets/team-shared-agents-cover.webp" width="1536" height="1024" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">02 · TEAM</span>
<h3>Everyone has AI. How does the team work together?</h3>
<p>Follow a team investigating a faulty digital piano. See how a shared agent uses teammates’ test results to continue the investigation.</p>
<span class="home-reading-card__cta">Read the team story →</span>
</a>
<a class="home-reading-card" href="/blog/what-is-holon">
<img class="home-reading-card__cover" src="/assets/holon-agent-workspace-cover.webp" width="1672" height="941" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">03 · PRODUCT</span>
<h3>Let multiple agents keep working in your environment</h3>
<p>Move from asking AI for help each time to giving agents ongoing roles. Meet the local workbench that supports this way of working.</p>
<span class="home-reading-card__cta">Meet Holon →</span>
</a>
</div>
<a class="home-reading-all" href="/blog/">View all articles →</a>
</section>

<section class="home-final">
<div>
<p class="home-kicker">Put your first agent to work</p>

## Start with one task you don't want to keep checking on.

Choose a task that needs follow-up. Define the outcome and when to ask for your input. As events and feedback arrive, your agent can keep working from its saved progress.

</div>
<div class="home-final__actions">
<a class="home-button" href="/getting-started/first-agent">Create your first agent</a>
<a href="/blog/">Read the practical guides →</a>
<a href="https://groups.google.com/g/holon-pioneers" target="_blank" rel="noreferrer">Join Holon Pioneers →</a>
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

- [Maintainers](./maintainers/)
  <!-- mdorigin:index kind=directory -->

- [Holon Mobile Privacy Policy](./privacy.md)
  Data handling in the Holon Android and iOS clients.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
