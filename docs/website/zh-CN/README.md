---
title: Holon
summary: 让项目工作在对话结束后继续推进。
order: 1
---

<div class="home-page">
<section class="home-hero">
<div class="home-hero__copy">

<p class="home-eyebrow">长期 Agent 的本地工作台</p>

# 让项目工作不止于一次对话。

<p class="home-hero__lede">让长期 Agent 在真实仓库和工具链中工作。Holon 保存工作状态，等待下一个有意义的事件，并从持久状态继续推进。</p>

<div class="home-actions">
<a class="home-button home-button--primary" href="/zh-CN/getting-started/">开始使用</a>
<a class="home-button home-button--quiet" href="https://github.com/holon-run/holon">在 GitHub 查看 ↗</a>
</div>

<p class="home-hero__note">Holon 是围绕 Agent 的本地优先运行时，不是另一个 Agent，也不是托管式 Agent 服务。</p>

</div>
<div class="home-runtime" aria-label="Holon 运行时全景占位图">
<div class="home-runtime__top"><span>运行时全景 · 内容占位</span><span class="home-runtime__status">本地运行</span></div>
<div class="home-runtime__events"><span>操作者输入</span><span>任务结果</span><span>外部事件</span><span>定时器</span></div>
<div class="home-runtime-map">
<div class="home-runtime-map__stage home-runtime-map__stage--core"><span class="home-runtime-map__label">长期 AGENT</span><strong>持续职责 + 持久工作状态</strong><small>运行时让身份和当前目标始终可寻址。</small></div>
<div class="home-runtime-map__connector" aria-hidden="true"><span>执行</span></div>
<div class="home-runtime-map__pair">
<div class="home-runtime-map__stage"><span class="home-runtime-map__label">工作环境</span><strong>仓库 + 工具</strong><small>工作发生在真实项目环境中。</small></div>
<div class="home-runtime-map__stage"><span class="home-runtime-map__label">控制边界</span><strong>信任 + 批准</strong><small>边界始终明确可见。</small></div>
</div>
<div class="home-runtime-map__connector" aria-hidden="true"><span>等待 · 唤醒 · 恢复</span></div>
<div class="home-runtime-map__stage home-runtime-map__stage--output"><span class="home-runtime-map__label">结果交付</span><strong>面向操作者的简报</strong><small>结果与内部执行痕迹相互分离。</small></div>
</div>
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
<p>在 Holon 中，工作项（WorkItem）承载需要持续推进的目标，记录计划、进度、等待条件和完成简报。暂停后继续时，Agent 沿着同一个工作项推进，而不是从聊天历史中重新梳理。</p>
<div class="home-state-sequence" aria-label="工作项生命周期"><span>事件</span><i>→</i><span>工作项</span><i>→</i><span>等待</span><i>→</i><span>唤醒</span><i>→</i><span>简报</span></div>
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
<p class="home-kicker">一个 Runtime · 多个入口</p>

## 界面可以关闭，工作仍有归处。

以后台服务运行时，Holon Runtime 承接 Agent、工作项和等待状态。TUI 与 Web UI 连接同一个 Runtime；关闭操作界面，不等于停止后台服务。

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

## 认识 Holon，再看它如何工作。

从产品介绍到工作项设计，再看审阅与小团队中的使用过程。首批四篇中文稿待审阅，英文暂留占位。

</div>
<a class="home-reading-all" href="/zh-CN/blog/">查看全部文章 →</a>
<div class="home-reading-grid">
<a class="home-reading-card" href="/zh-CN/blog/what-is-holon">
<span class="home-reading-card__category">01 · 产品故事</span>
<h3>CI 还没结束，你要下班了</h3>
<p>从一次尚未完成的 PR 出发，看 Holon 为什么把工作留在常驻 Runtime 中，而不只留在对话里。</p>
<span class="home-reading-card__cta">认识 Holon →</span>
</a>
<a class="home-reading-card" href="/zh-CN/blog/why-work-items">
<span class="home-reading-card__category">02 · 技术设计</span>
<h3>工作暂停之后，Runtime 保存什么？</h3>
<p>拆开 WorkItem、任务和等待条件，看工作如何让出执行机会，又如何在事件到来后继续。</p>
<span class="home-reading-card__cta">拆解 WorkItem →</span>
</a>
<a class="home-reading-card" href="/zh-CN/blog/one-pr-one-work-item">
<span class="home-reading-card__category">03 · 审阅实践</span>
<h3>一个 PR，一个工作项</h3>
<p>从授权与首次审阅开始，把新提交、CI 和复查接成一条持续工作流。附任务指令与检查步骤。</p>
<span class="home-reading-card__cta">配置持续审阅 →</span>
</a>
<a class="home-reading-card" href="/zh-CN/blog/agents-in-a-small-team">
<span class="home-reading-card__category">04 · 团队协作</span>
<h3>个人有了 AI，团队怎样一起工作？</h3>
<p>从设备故障调查到琴键无声的真机验证，看共享 Agent 怎样接入团队分工，接回人的测试结果。</p>
<span class="home-reading-card__cta">看团队如何协作 →</span>
</a>
</div>
</section>

<section class="home-section home-start-section">
<div class="home-start">
<div class="home-start__copy">
<p class="home-kicker">从本地开始</p>

## 先交给 Holon 一项有用的职责。

安装 Holon、配置模型提供商并启动 daemon。打开本地 Web 界面 `http://localhost:7878`，或使用 `holon tui`。

<p class="home-start__boundary">早期软件 · 显式信任边界 · 人工批准始终可见</p>

<div class="home-actions"><a class="home-button home-button--primary" href="/zh-CN/getting-started/">查看完整开始路径</a></div>

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

## 给项目工作一个持久归属。

从本地开始，让第一条工作流保持小而清楚；当聊天或终端结束时，由 Holon 保存工作本身。

<small class="home-translation-note">中文文档正在逐步完善中。尚未翻译的页面会链接到对应的英文版本。</small>

</div>
<div class="home-final__actions">
<a class="home-button" href="/zh-CN/getting-started/">开始使用</a>
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
