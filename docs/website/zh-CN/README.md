---
title: Holon
summary: Agent 主动工作，持续跟进。Holon 是可部署在个人电脑或团队服务器上的 Agent 工作台；接入系统事件和定时安排，让 Agent 按约定行动、保留进度，并在需要时请你参与。
order: 1
---

<div class="home-page home-page--zh">
<section class="home-hero">
<div class="home-hero__copy">

<p class="home-eyebrow">为需要持续跟进的工作而建</p>

# Agent 主动工作，<br>持续跟进。

<p class="home-hero__lede">Holon 是可部署在个人电脑或团队服务器上的 Agent 工作台。从邮件处理、订单异常到代码审阅，接入系统事件或定时安排，让 Agent 按约定行动、保留进度，并在需要时请你参与。</p>

<div class="home-actions">
<a class="home-button home-button--primary" href="#install">开始使用</a>
<a class="home-button home-button--quiet" href="#capabilities">了解如何工作 ↓</a>
</div>

<p class="home-hero__note">你设定职责与权限，Agent 按约定工作。</p>

<div class="home-litepaper" role="group" aria-label="Holon 产品概览 PDF">
<span class="home-litepaper__label">产品概览 · 6 页 PDF</span>
<a href="/assets/lite-paper/holon-lite-paper-zh-CN.pdf" type="application/pdf" hreflang="zh-CN" aria-label="阅读中文版 Holon Lite Paper（PDF）">阅读 <span aria-hidden="true">↗</span></a>
</div>

</div>
<div class="home-runtime" aria-label="在约定的职责与权限内，任务安排、系统事件和定时安排触发 Agent 开始或继续工作；Agent 保留进度，等待结果，需要判断或授权时请你参与。">
<div class="home-runtime__top"><span>按约定行动 · 沿着进度继续</span><span class="home-runtime__status">示意图</span></div>
<div class="home-followup__entry"><strong>你设定职责、权限与交付要求</strong><span>以下输入可触发开始或继续工作 ↓</span></div>
<div class="home-trigger-sources" role="group" aria-label="工作触发方式"><div><strong>任务与反馈</strong><span>安排任务 · 补充信息</span></div><div><strong>系统事件</strong><span>接入邮件 · 订单 · CI</span></div><div><strong>定时安排</strong><span>定期检查 · 汇总跟进</span></div></div>
<div class="home-followup">
<div class="home-followup__cycle" role="group" aria-label="顺时针循环：推进工作，记录进展，等待变化，接着推进">
<div class="home-followup__step home-followup__step--work"><strong>推进工作</strong><small>在实际环境里做事</small></div>
<span class="home-followup__arrow home-followup__arrow--top" aria-hidden="true">→</span>
<div class="home-followup__step home-followup__step--record"><strong>记录进展</strong><small>保留状态与下一步</small></div>
<span class="home-followup__arrow home-followup__arrow--right" aria-hidden="true">↓</span>
<div class="home-followup__center"><strong>Agent 持续跟进</strong><span>沿着同一项工作继续</span></div>
<div class="home-followup__step home-followup__step--wait"><strong>等待变化</strong><small>等结果或约定条件</small></div>
<span class="home-followup__arrow home-followup__arrow--bottom" aria-hidden="true">←</span>
<div class="home-followup__step home-followup__step--resume"><strong>接着推进</strong><small>条件满足后继续</small></div>
<span class="home-followup__arrow home-followup__arrow--left" aria-hidden="true">↑</span>
</div>
<aside class="home-followup__human"><span aria-hidden="true">⇄</span><strong>你按需参与</strong><p>需要判断<br>或授权时</p><small>确认后<br>回到工作中</small></aside>
</div>
<p class="home-followup__note">收到事件或到了约定时间，按需开始或继续。<br><span>任务完成后，交付结果。</span></p>
</div>
</section>

<section class="home-section home-capability-section" id="capabilities">
<div class="home-section__intro">
<p class="home-kicker">Holon 如何工作</p>

## 开始处理，等待结果，接着推进。

需要持续跟进的工作，往往要经历多次行动与等待。Holon 保留每一步进展，让 Agent 在收到新信息后接着做，也让你随时了解工作到了哪一步。

</div>
<div class="home-capability-system">
<article class="home-capability-main">
<div class="home-capability-main__heading"><span>01</span><small>WorkItem｜工作项</small></div>
<h3>保留目标和进度，回来接着推进。</h3>
<p>Holon 用工作项（WorkItem）保存目标、计划、进度，以及正在等待的结果。无论是等订单状态更新，还是等同事完成测试，收到反馈后，Agent 都能沿着已有进度继续处理。</p>
<div class="home-state-sequence" aria-label="持续协作过程"><span>定目标</span><i>→</i><span>记进展</span><i>→</i><span>等变化</span><i>→</i><span>接着做</span><i>→</i><span>交结果</span></div>
<footer>WorkItem · 目标 · 计划 · 进度 · 等待条件 · 完成简报</footer>
</article>
<div class="home-capability-support">
<article class="home-capability-row"><span>02</span><div><small>事件与定时安排</small><h3>响应新消息，也按时行动。</h3><p>通过 Webhook 接入外部系统事件，或设置定时任务，让 Agent 按约定开始处理。工作中也可以等待结果或反馈，条件满足后再继续。</p></div></article>
<article class="home-capability-row"><span>03</span><div><small>职责与记忆</small><h3>让同一个 Agent 持续负责。</h3><p>为每个 Agent 定义职责，单独配置技能（Skills）和指令，保留各自的记忆。下次回来，仍然可以找它继续处理负责的事务。</p></div></article>
<article class="home-capability-row"><span>04</span><div><small>多 Agent 分工</small><h3>分配任务，汇集结果。</h3><p>把调查、审阅、测试等任务交给不同 Agent，也可以创建子 Agent 并行处理。跟踪各项任务，收到结果后继续推进整体工作。</p></div></article>
</div>
</div>
</section>

<section class="home-section home-product">
<div class="home-section__intro home-section__intro--narrow">
<p class="home-kicker">个人使用，也能团队共享</p>

## 随时查看进展，一起把工作往前推。

通过 Web 界面或终端界面（TUI）查看任务、补充信息、调整方向。部署到团队服务器后，成员可以向同一个 Agent 安排任务、反馈结果，沿着已有进度继续工作。

多个工作区分别存放不同项目的文件与产物。Agent 在后台跟进，你和团队按需回来查看与参与。

</div>
<figure class="home-workbench-preview">
<a href="/assets/lite-paper/web-gui-review-zh-CN.png" aria-label="查看 Web 工作台截图大图">
<img src="/assets/lite-paper/web-gui-review-zh-CN.png" width="3000" height="1880" alt="Holon Web 工作台：左侧按职责列出 Agent，中间展示审阅进展与下一步，右侧显示当前任务、等待原因与待办。" loading="lazy" decoding="async">
</a>
<figcaption>Web 工作台：查看已完成的工作、正在等待的结果和下一步。截图使用演示数据，点击可查看大图。</figcaption>
</figure>
<p class="home-product__caption">持续跟进需要运行 Holon 的电脑或服务器及后台服务保持开启。Agent 可使用的文件与工具取决于你的配置。</p>
</section>

<section class="home-section home-proof">
<div class="home-proof__intro">
<p class="home-kicker">精选文章</p>

## 从理解 Holon，到用它做事。

看看团队怎样共享 Agent，动手配置持续审阅，再了解支撑持续跟进的设计。

</div>
<a class="home-reading-all" href="/zh-CN/blog/">查看全部文章 →</a>
<div class="home-reading-grid">
<a class="home-reading-card" href="/zh-CN/blog/what-is-holon">
<img class="home-reading-card__cover" src="/assets/holon-agent-workspace-cover.webp" width="1672" height="941" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">01 · 产品故事</span>
<h3>让多个 Agent 在你的工作环境里持续做事</h3>
<p>从每次叫 AI 帮忙，到让固定角色持续跟进。认识支撑这种协作方式的本地工作台。</p>
<span class="home-reading-card__cta">认识 Holon →</span>
</a>
<a class="home-reading-card" href="/zh-CN/blog/agents-in-a-small-team">
<img class="home-reading-card__cover" src="/assets/team-shared-agents-cover.webp" width="1536" height="1024" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">02 · 团队协作</span>
<h3>个人有了 AI，团队怎样一起工作？</h3>
<p>从设备故障调查到琴键无声的真机验证，看团队怎样共享 Agent，并根据测试反馈继续调查。</p>
<span class="home-reading-card__cta">看团队如何协作 →</span>
</a>
<a class="home-reading-card" href="/zh-CN/blog/one-pr-one-work-item">
<img class="home-reading-card__cover" src="/assets/continuous-pr-reviewer-cover.webp" width="1672" height="941" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">03 · 审阅实践</span>
<h3>搭建持续跟进 PR 的 Reviewer</h3>
<p>从 code-reviewer 模板开始，确认职责与权限，订阅仓库 PR，持续跟进修复、CI 和合并。</p>
<span class="home-reading-card__cta">配置持续审阅 →</span>
</a>
<a class="home-reading-card" href="/zh-CN/blog/why-work-items">
<img class="home-reading-card__cover" src="/assets/workitem-abstract-flow-cover.webp" width="1536" height="768" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">04 · 技术设计</span>
<h3>WorkItem：把工作状态从对话中分离</h3>
<p>从接口迁移到两个 PR 交替推进，看看目标、进度和等待条件如何跨轮保留。</p>
<span class="home-reading-card__cta">拆解 WorkItem →</span>
</a>
</div>
</section>

<section class="home-section home-start-section" id="install">
<div class="home-start">
<div class="home-start__copy">
<p class="home-kicker">从本地开始</p>

## 从一个模板，一项任务开始。

从 Agent 模板（Agent Template）创建 Agent，明确职责并安排第一项任务。再根据需要接入系统事件、设置定时任务，让它持续跟进。

先安装 Holon、配置模型并启动后台服务。然后打开 Web 界面 `http://localhost:7878`，或运行 `holon tui`。

<p class="home-start__boundary">推荐版本：<a href="https://github.com/holon-run/holon/releases/tag/v0.44.1">v0.44.1</a></p>

<div class="home-actions">
<a class="home-button home-button--primary" href="/zh-CN/getting-started/">完整安装指南</a>
<a class="home-button home-button--quiet" href="https://github.com/holon-run/holon/releases">下载二进制 ↗</a>
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
<p class="home-kicker">从这里继续</p>

## 先交给 Agent 一件你不想反复盯着的事。

选一件需要持续跟进的工作，约定交付结果和需要你参与的时刻。从第一次安排，到下一次事件或反馈，让 Agent 沿着进度继续。

</div>
<div class="home-final__actions">
<a class="home-button" href="/zh-CN/getting-started/first-agent">创建第一个 Agent</a>
<a href="/zh-CN/blog/">阅读实践文章 →</a>
</div>
</section>
</div>

<!-- INDEX:START -->

- [运行时规格](./spec/)
  <!-- mdorigin:index kind=directory -->

- [博客](./blog/)
  <!-- mdorigin:index kind=directory -->

- [开始使用](./getting-started/)
  <!-- mdorigin:index kind=directory -->

- [概念](./concepts/)
  <!-- mdorigin:index kind=directory -->

- [指南](./guides/)
  <!-- mdorigin:index kind=directory -->

- [参考](./reference/)
  <!-- mdorigin:index kind=directory -->

- [维护者](./maintainers/)
  <!-- mdorigin:index kind=directory -->

<!-- INDEX:END -->
