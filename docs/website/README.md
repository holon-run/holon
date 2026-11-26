---
title: Open-source AI assistants on your own computer
summary: "Open-source, local-first Rust workbench for always-on AI agents — explicit work items, event-driven wait/wake, and reliable follow-through."
order: 1
---

<div class="home-page home-page--en">
<section class="home-hero">
<div class="home-hero__copy">

<p class="home-eyebrow">Built for work that needs follow-through</p>

# Create and run AI assistants on your own computer or server.

<p class="home-hero__lede">Open-source, local-first Rust workbench for always-on AI agents. Assign explicit work items, preserve durable state across sessions, and let agents wait on events and follow through.</p>

<div class="home-actions">
<a class="home-button home-button--primary" href="#install">Install and start</a>
<a class="home-button home-button--quiet" href="#capabilities">See how it works ↓</a>
</div>

<div class="home-mobile" id="clients" role="group" aria-labelledby="clients-title">
<div class="home-mobile__header">
<h2 id="clients-title">Clients &amp; apps</h2>
<span>Connect to your running Holon daemon</span>
</div>
<div class="home-client-pills">
<a class="home-pill" href="https://apps.apple.com/us/app/holon-agent-remote/id6819978897">iOS · App Store ↗</a>
<a class="home-pill" href="https://github.com/holon-run/holon/releases/download/v0.61.1/holon-android-v0.61.1.apk">Android APK ↗</a>
<a class="home-pill" href="https://github.com/holon-run/holon/releases/latest">macOS Menu Bar (.dmg) ↗</a>
</div>
<p class="home-mobile__links"><a href="https://testflight.apple.com/join/XkTfN12S">iOS beta via TestFlight ↗</a> · <a href="/concepts/architecture-comparison">Architecture comparison →</a> · <a href="/assets/lite-paper/holon-lite-paper-en.pdf" type="application/pdf" hreflang="en" aria-label="Read the Holon lite paper in English (PDF)">Lite paper (PDF) ↗</a></p>
</div>

</div>
<div class="home-stage" data-stage-tabs>
<div class="home-stage__bar">
<div class="home-stage-tablist" role="tablist" aria-label="Workbench interfaces" hidden>
<button type="button" role="tab" data-stage-tab="web" aria-selected="true">Web Workbench</button>
<button type="button" role="tab" data-stage-tab="tui" aria-selected="false">Terminal · TUI &amp; CLI</button>
<button type="button" role="tab" data-stage-tab="mobile" aria-selected="false">Mobile · iOS &amp; Android</button>
</div>
</div>
<div class="home-stage__panel" data-stage-panel="web">
<a class="home-stage__shot" href="/assets/home-workbench-en.webp" aria-label="View the Holon web workbench screenshot at full size">
<img src="/assets/home-workbench-en.webp" width="1600" height="1003" alt="Holon web workbench: Holon Bot coordinates Leo (Code Review), Maya (Docs), and Nora (Growth) on the left, summarizes completed docs and launch drafts in the center, and tracks the v0.62 release WorkItem checklist and CI wait state on the right." fetchpriority="high" decoding="async">
</a>
</div>
<div class="home-stage__panel" data-stage-panel="tui" hidden>
<div class="home-terminal" role="region" aria-label="Terminal CLI and TUI session inspecting Holon Bot coordinating the v0.62 launch WorkItem">
<div class="home-terminal__chrome" aria-hidden="true"><span class="home-terminal__dots"><i></i><i></i><i></i></span><span class="home-terminal__title">holon — local daemon · 127.0.0.1:7878</span></div>
<div class="home-terminal__body">
<div class="home-terminal__line"><span class="home-terminal__prompt">$</span> <span class="home-terminal__cmd">holon work-item list</span></div>
<div class="home-terminal__line home-terminal__line--head">WORK ITEM              AGENT          STATUS    TITLE</div>
<div class="home-terminal__line">launch-v062            holonbot       <span class="home-terminal__badge home-terminal__badge--wait">blocked</span>   Coordinate v0.62 launch: code, docs &amp; content</div>
<div class="home-terminal__line">demo-leo-work          leo-reviewer   <span class="home-terminal__badge home-terminal__badge--wait">blocked</span>   Follow PR #128: waiting on release CI checks</div>
<div class="home-terminal__spacer"></div>
<div class="home-terminal__line"><span class="home-terminal__prompt">$</span> <span class="home-terminal__cmd">holon tail --agent holonbot --include-progress</span></div>
<div class="home-terminal__line"><span class="home-terminal__time">10:11</span> <span class="home-terminal__tag home-terminal__tag--ok">maya-docs</span>    Updated quickstart &amp; mobile QR pairing guides</div>
<div class="home-terminal__line"><span class="home-terminal__time">10:13</span> <span class="home-terminal__tag home-terminal__tag--ok">nora-growth</span>  Drafted v0.62 launch post &amp; release highlights</div>
<div class="home-terminal__line"><span class="home-terminal__time">10:15</span> <span class="home-terminal__tag home-terminal__tag--wait">parked</span>       Waiting on Leo (PR #128 CI) &amp; your approval to publish</div>
<div class="home-terminal__spacer"></div>
<div class="home-terminal__line"><span class="home-terminal__prompt">$</span> <span class="home-terminal__cmd">holon tui</span></div>
<div class="home-terminal__line home-terminal__line--muted">▸ Attached to local runtime · 4 agents online · 2 todos done, waiting on CI &amp; sign-off</div>
</div>
</div>
</div>
<div class="home-stage__panel" data-stage-panel="mobile" hidden>
<div class="home-mobile-stage" role="region" aria-label="Holon mobile client connected to Holon Bot and specialist agents">
<div class="home-phones" aria-hidden="true">
<div class="home-phone">
<div class="home-phone__notch"><span>9:41</span><i></i><span>5G</span></div>
<div class="home-phone__top"><strong>Holon</strong><span class="home-phone__live">dev-server</span></div>
<div class="home-phone__search">Filter agents...</div>
<div class="home-phone__list">
<div class="home-phone__row home-phone__row--active"><span class="home-phone__dot home-phone__dot--wait"></span><div><strong>Holon Bot</strong><small>Coordinate v0.62 launch: code...</small></div><em>Wait</em></div>
<div class="home-phone__row"><span class="home-phone__dot home-phone__dot--wait"></span><div><strong>Leo · Code Review</strong><small>Follow PR #128: waiting on CI</small></div><em>CI</em></div>
<div class="home-phone__row"><span class="home-phone__dot home-phone__dot--ok"></span><div><strong>Maya · Docs</strong><small>Pairing &amp; CLI guides updated</small></div></div>
<div class="home-phone__row"><span class="home-phone__dot home-phone__dot--ok"></span><div><strong>Nora · Growth</strong><small>Launch post &amp; social draft ready</small></div></div>
</div>
</div>
<div class="home-phone">
<div class="home-phone__notch"><span>9:42</span><i></i><span>5G</span></div>
<div class="home-phone__top"><span>‹ Holon Bot</span><span class="home-phone__chip">Files</span></div>
<div class="home-phone__card">
<span class="home-phone__kicker">V0.62 LAUNCH · BRIEF</span>
<strong>Docs &amp; launch draft ready; waiting on CI</strong>
<p>Maya updated the guides and Nora drafted the launch post. Only PR #128 with Leo is waiting on CI.</p>
<div class="home-phone__trace">▸ 3 role updates &amp; checks collapsed</div>
</div>
<div class="home-phone__wait">Waiting on PR #128 CI &amp; publish sign-off</div>
<div class="home-phone__composer"><span>Publish once Leo confirms CI passes</span><b>↑</b></div>
</div>
</div>
<div class="home-mobile-stage__info">
<span class="home-mobile-stage__kicker">HOLON AGENT REMOTE · IOS &amp; ANDROID</span>
<h3>Step away from your desk without pausing the team.</h3>
<ul class="home-mobile-stage__list">
<li><strong>QR code pairing</strong> — Scan once from Web UI or macOS menu bar; revocable credentials stay in system Keychain.</li>
<li><strong>Result-first timeline</strong> — Read clean briefs and draft files from Holon Bot, Maya, or Nora while raw logs stay collapsed.</li>
<li><strong>Approve on the go</strong> — Sign off on a launch post or steer an ongoing WorkItem in one reply from anywhere.</li>
</ul>
<div class="home-mobile-stage__links">
<a href="https://apps.apple.com/us/app/holon-agent-remote/id6819978897">iOS App Store ↗</a>
<a href="https://github.com/holon-run/holon/releases/download/v0.61.1/holon-android-v0.61.1.apk">Android APK ↗</a>
<a href="/guides/connect-ios-client">iOS pairing guide →</a>
</div>
</div>
</div>
</div>
<div class="home-stage__callouts">
<div class="home-stage__callout"><span>01 · COORDINATED ROLES</span><strong>Holon Bot + named specialists</strong><p>Start with Holon Bot, then delegate recurring code review, docs, and growth work to dedicated agents.</p></div>
<div class="home-stage__callout"><span>02 · WORKITEM</span><strong>Saved plan &amp; checklist</strong><p>Goals, completed deliverables, and pending handoffs stay attached to one task across turns and days.</p></div>
<div class="home-stage__callout"><span>03 · WAIT / WAKE</span><strong>Park on CI or human sign-off</strong><p>Agents sleep cleanly while waiting on webhooks, teammates, or your approval, then resume in place.</p></div>
</div>
</div>
</section>

<section class="home-section home-start-section" id="install">
<div class="home-start">
<div class="home-start__copy">
<p class="home-kicker">Quick start</p>

## Install. Configure. Put an assistant to work.

Install Holon, then run `holon onboard` to configure a model provider and `holon daemon start` to start the background service.

Open `http://localhost:7878` or run `holon tui` to create your first assistant and assign a task.

<p class="home-start__boundary">Recommended release: <a href="https://github.com/holon-run/holon/releases/tag/v0.61.1">v0.61.1</a></p>

<div class="home-actions">
<a class="home-button home-button--primary" href="/getting-started/">Setup guide · all platforms</a>
<a class="home-button home-button--quiet" href="https://github.com/holon-run/holon/releases">Download binaries ↗</a>
</div>

</div>

<div class="home-start__commands" data-install-tabs>
<div class="home-install-tablist" role="tablist" aria-label="Installation methods" hidden>
<button type="button" role="tab" data-install-tab="brew" aria-selected="true">macOS · Homebrew</button>
<button type="button" role="tab" data-install-tab="docker" aria-selected="false">Docker Compose</button>
<button type="button" role="tab" data-install-tab="binary" aria-selected="false">Linux &amp; Binary</button>
</div>
<div class="home-install-panel" data-install-panel="brew">
<p class="home-start__command-label">macOS · Homebrew</p>

```bash
brew tap holon-run/tap && brew install holon
holon onboard
holon daemon start
```

</div>
<div class="home-install-panel" data-install-panel="docker" hidden>
<p class="home-start__command-label">Docker Compose · Browser setup</p>

```bash
git clone https://github.com/holon-run/holon.git && cd holon
docker compose up -d
# Open http://localhost:7878/settings to configure a model provider
```

</div>
<div class="home-install-panel" data-install-panel="binary" hidden>
<p class="home-start__command-label">Linux amd64 &amp; macOS · Prebuilt release</p>

```bash
# Download from https://github.com/holon-run/holon/releases/latest
chmod +x holon && mv holon ~/.local/bin/
holon onboard && holon daemon start
```

</div>
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

## One runtime. Any client. Shared across your team.

Run Holon on your laptop or a shared Linux server. Connect from the browser, terminal (TUI), or phone to inspect progress, supply context, and steer ongoing work without losing state.

</div>
<figure class="home-architecture-figure">
<picture class="home-architecture">
<source media="(max-width: 640px)" srcset="/assets/runtime-architecture-en-narrow.png" width="480" height="550">
<img src="/assets/runtime-architecture-en.png" width="1200" height="350" alt="Holon runtime architecture: TUI, Web UI, and Mobile interfaces connect to the Holon service on your host machine, managing Agents, WorkItems, and Wait/Wake lifecycles while executing inside authorized project workspaces." loading="lazy" decoding="async">
</picture>
</figure>
<div class="home-team-points">
<div><h3>Switch interfaces freely</h3><p>Start a task in the terminal, check status from iOS or Android while away, and review diffs in the Web UI—all against the same live daemon.</p></div>
<div><h3>Collaborate on shared agents</h3><p>On a team server, teammates can assign tasks to a shared agent, drop in test results, and continue from the saved WorkItem history.</p></div>
<div><h3>Scoped project workspaces</h3><p>Keep repositories, files, and outputs separated per workspace. Your configuration controls which tools and directories each agent can touch.</p></div>
</div>
<p class="home-product__caption">Ongoing work requires the host computer or server and the Holon background daemon to stay running.</p>
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
<a class="home-reading-card" href="/blog/one-pr-one-work-item">
<img class="home-reading-card__cover" src="/assets/continuous-pr-reviewer-cover.webp" width="1600" height="900" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">01 · WORKFLOW</span>
<h3>Build a reviewer that follows PRs through</h3>
<p>Set up the code-reviewer template, agree on permissions, and follow revisions and CI checks through to the merge decision.</p>
<span class="home-reading-card__cta">Set up continuous review →</span>
</a>
<a class="home-reading-card" href="/blog/agents-in-a-small-team">
<img class="home-reading-card__cover" src="/assets/team-shared-agents-cover.webp" width="1536" height="1024" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">02 · TEAM</span>
<h3>Everyone has AI. How does the team work together?</h3>
<p>Follow a team investigating a faulty digital piano. See how a shared agent uses teammates’ test results to continue the investigation.</p>
<span class="home-reading-card__cta">Read the team story →</span>
</a>
<a class="home-reading-card" href="/blog/what-is-holon">
<img class="home-reading-card__cover" src="/assets/holon-agent-workspace-cover.webp" width="1600" height="900" alt="" loading="lazy" decoding="async">
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
