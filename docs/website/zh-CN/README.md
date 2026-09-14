---
title: Holon
summary: Agent 持续跟进，无需事事盯着。Holon 是为持续工作而建的本地 Agent 工作台；你决定目标和边界，Agent 记录进展、等待变化，再接着推进。
order: 1
---

<div class="home-page home-page--zh">
<section class="home-hero">
<div class="home-hero__copy">

<p class="home-eyebrow">从一次对话，到长期协作</p>

# Agent 持续跟进，<br>无需事事盯着。

<p class="home-hero__lede">把需要长期关注的事务交给 Agent。你设定目标和边界，Agent 按约定持续跟进，有变化时继续推进，需要你判断或授权时再请你参与。</p>

<div class="home-actions">
<a class="home-button home-button--primary" href="#install">开始使用</a>
<a class="home-button home-button--quiet" href="#capabilities">了解如何协作 ↓</a>
</div>

<p class="home-hero__note">Holon，为持续工作而建的本地 Agent 工作台。</p>

</div>
<div class="home-runtime" aria-label="你设定目标和边界，Agent 在推进工作、记录进展、等待变化与接着推进之间循环；需要判断或授权时请你参与，任务完成后交付。">
<div class="home-runtime__top"><span>你与 Agent · 持续协作</span><span class="home-runtime__status">示意图</span></div>
<div class="home-followup__entry"><strong>你设定目标和边界</strong><span>约定权限 · 明确交付</span></div>
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
<p class="home-followup__note">等待时暂停，有变化再继续。<br><span>任务完成后，交付结果。</span></p>
</div>
</section>

<section class="home-section home-capability-section" id="capabilities">
<div class="home-section__intro">
<p class="home-kicker">Holon 如何工作</p>

## 一个核心工作模型，由三项运行时能力支撑。

用明确的目标和进度组织工作。由长期负责的 Agent 持续推进，等待条件满足后继续，需要分工时再委派任务。

</div>
<div class="home-capability-system">
<article class="home-capability-main">
<div class="home-capability-main__heading"><span>01</span><small>WorkItem｜工作项</small></div>
<h3>让目标、进度和等待都有明确归属。</h3>
<p>一项需要长期跟进的任务，不该只留在聊天记录里。Holon 用工作项（WorkItem）保留目标、计划、进度和等待条件。Agent 暂停后继续时，能沿着同一项工作接着推进。</p>
<div class="home-state-sequence" aria-label="持续协作过程"><span>定目标</span><i>→</i><span>记进展</span><i>→</i><span>等变化</span><i>→</i><span>接着做</span><i>→</i><span>交结果</span></div>
<footer>WorkItem · 目标 · 计划 · 进度 · 等待条件 · 完成简报</footer>
</article>
<div class="home-capability-support">
<article class="home-capability-row"><span>02</span><div><small>事件驱动续接</small><h3>有条件地等待，有信号再继续。</h3><p>明确记录工作项在等什么：任务结果、外部事件、定时器，或你的输入。条件满足后，继续推进对应工作项。</p></div></article>
<article class="home-capability-row"><span>03</span><div><small>长期职责身份</small><h3>延续负责的 Agent，而不只是一段对话。</h3><p>为每个 Agent 设定持续职责，保留各自的指令与持久记忆。跨会话回到同一个 Agent，继续它负责的工作。</p></div></article>
<article class="home-capability-row"><span>04</span><div><small>多 Agent 分工协作</small><h3>分头处理，接续推进。</h3><p>把任务交给已有 Agent，或创建子 Agent 并行处理。跟踪任务进度，等待结果返回，再继续推进主线工作。</p></div></article>
</div>
</div>
</section>

<section class="home-section home-product">
<div class="home-section__intro home-section__intro--narrow">
<p class="home-kicker">持续协作背后的工作台</p>

## 不必守着对话窗口，工作仍有归处。

Holon 不是另一个 Agent，而是让 Agent 持续工作的本地运行时。以后台服务运行时，它保存工作状态、组织等待与唤醒。你可以通过 TUI 或 Web UI 回来查看进展、补充信息或调整方向。

</div>
<picture class="home-architecture">
<source media="(max-width: 1100px)" srcset="/assets/runtime-architecture-zh-narrow.png" width="480" height="550">
<img src="/assets/runtime-architecture-zh.png" width="1200" height="350" alt="TUI 与 Web UI 连接同一常驻 Holon，手机入口以虚线边框和连线表示。Holon 支持持续工作、状态保存与事件唤醒，管理 Agent 和工作项，在宿主机上使用工作区、文件与工具链。" loading="lazy" decoding="async">
</picture>
<p class="home-product__caption">持续执行依赖 daemon 与宿主机保持运行；机器边界说明运行位置，不代表额外的权限隔离。</p>
</section>

<section class="home-section home-proof">
<div class="home-proof__intro">
<p class="home-kicker">精选文章</p>

## 从理解 Holon，到用它做事。

了解背后的设计，动手配置持续审阅，探索人与 Agent 的协作方式。

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
<a class="home-reading-card" href="/zh-CN/blog/why-work-items">
<img class="home-reading-card__cover" src="/assets/workitem-abstract-flow-cover.webp" width="1536" height="768" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">02 · 技术设计</span>
<h3>WorkItem：把工作状态从对话中分离</h3>
<p>从接口迁移到两个 PR 交替推进，看看目标、进度和等待条件如何跨轮保留。</p>
<span class="home-reading-card__cta">拆解 WorkItem →</span>
</a>
<a class="home-reading-card" href="/zh-CN/blog/one-pr-one-work-item">
<img class="home-reading-card__cover" src="/assets/continuous-pr-reviewer-cover.webp" width="1672" height="941" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">03 · 审阅实践</span>
<h3>搭建持续跟进 PR 的 Reviewer</h3>
<p>从 code-reviewer 模板开始，确认职责与权限，订阅仓库 PR，持续跟进修复、CI 和合并。</p>
<span class="home-reading-card__cta">配置持续审阅 →</span>
</a>
<a class="home-reading-card" href="/zh-CN/blog/agents-in-a-small-team">
<img class="home-reading-card__cover" src="/assets/team-shared-agents-cover.webp" width="1536" height="1024" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">04 · 团队协作</span>
<h3>个人有了 AI，团队怎样一起工作？</h3>
<p>从设备故障调查到琴键无声的真机验证，看共享 Agent 怎样接入团队分工，接回人的测试结果。</p>
<span class="home-reading-card__cta">看团队如何协作 →</span>
</a>
</div>
</section>

<section class="home-section home-start-section" id="install">
<div class="home-start">
<div class="home-start__copy">
<p class="home-kicker">从本地开始</p>

## 为第一项持续任务做好准备。

安装 Holon、配置模型提供商并启动 daemon。打开本地 Web 界面 `http://localhost:7878`，或使用 `holon tui`。

<p class="home-start__boundary">推荐版本：<a href="https://github.com/holon-run/holon/releases/tag/v0.40.0">v0.40.0</a></p>

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

从一个明确的目标开始，约定跟进方式、交付结果和需要你参与的时刻。少一些反复检查和催促，把时间留给你真正想做的事。

<small class="home-translation-note">中文文档正在逐步完善中。尚未翻译的页面会链接到对应的英文版本。</small>

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

<!-- INDEX:END -->
