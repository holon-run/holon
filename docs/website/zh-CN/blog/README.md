---
title: 博客
summary: 认识 Holon，了解它的设计，看看长期 Agent 怎样参与真实工作。
order: 4
---

<div class="blog-page">
<header class="blog-intro">
<p class="blog-eyebrow">HOLON 博客</p>

# <span class="blog-title-line">工作，</span><span class="blog-title-line">接着往下做。</span>

<p class="blog-intro__lede">从产品思考到运行时设计，再到把 Agent 用起来的人和团队。</p>
</header>

<a class="blog-feature" href="/zh-CN/blog/what-is-holon">
<img src="/assets/holon-agent-workspace-cover.webp" width="1672" height="941" alt="" decoding="async" fetchpriority="high">
<div class="blog-feature__copy">
<span class="blog-category">从这里开始 · 产品介绍</span>
<h2>Holon 是什么：让多个 Agent 在你的工作环境里持续做事</h2>
<p>把反复发生的工作交给固定角色，在后台跟进，需要时接入。从持续审阅出发，认识这套本地工作台及其在远程开发和团队协作中的用法。</p>
<span class="blog-read">认识 Holon <span aria-hidden="true">→</span></span>
</div>
</a>

<section class="blog-stories" aria-labelledby="blog-stories-title">
<div class="blog-section-heading">
<h2 id="blog-stories-title">设计与实践</h2>
<p>理解它如何工作，再把它用起来。</p>
</div>
<a class="blog-story" href="/zh-CN/blog/why-work-items">
<img src="/assets/workitem-abstract-flow-cover.webp" width="1536" height="768" alt="" loading="lazy" decoding="async">
<div><span class="blog-category">技术设计</span>
<h3>WorkItem 架构：把工作状态从对话中分离</h3>
<p>从接口迁移到两个 PR 交替推进，看目标、进度和等待条件为什么需要独立于对话保存。</p>
<span class="blog-read">拆解 WorkItem <span aria-hidden="true">→</span></span></div>
</a>
<a class="blog-story" href="/zh-CN/blog/one-pr-one-work-item">
<img src="/assets/continuous-pr-reviewer-cover.webp" width="1672" height="941" alt="" loading="lazy" decoding="async">
<div><span class="blog-category">操作指南</span>
<h3>不止审一次代码：搭建持续跟进 PR 的 Reviewer</h3>
<p>从 code-reviewer 模板开始，确认权限，订阅仓库 PR。让同一个 reviewer 跟进修订与 CI，直到可以做出合并决定。</p>
<span class="blog-read">配置持续审阅 <span aria-hidden="true">→</span></span></div>
</a>
<a class="blog-story" href="/zh-CN/blog/agents-in-a-small-team">
<img src="/assets/team-shared-agents-cover.webp" width="1536" height="1024" alt="" loading="lazy" decoding="async">
<div><span class="blog-category">团队案例</span>
<h3>从个人 AI 工具到团队协作：一个小团队的 Agent Native 实践</h3>
<p>77 天的留存用量、设备故障调查和真机测试，记录共享 Agent 怎样接入团队分工，以及哪些环节仍需要人。</p>
<span class="blog-read">看团队如何协作 <span aria-hidden="true">→</span></span></div>
</a>
</section>

<footer class="blog-next">
<div><h2>先交给它一项明确的职责。</h2><p>安装 Holon、配置模型提供商，为第一个 Agent 划定工作范围。</p></div>
<a href="/zh-CN/getting-started/">开始使用 <span aria-hidden="true">→</span></a>
</footer>
</div>

<!-- INDEX:START -->

- [Holon 是什么：让多个 Agent 在你的工作环境里持续做事](./what-is-holon.md)
  把反复发生的工作交给固定角色，在后台跟进，需要时接入。通过持续审阅的例子，认识 Holon 这套本地工作台及其在远程开发和团队协作中的用法。
  <!-- mdorigin:index kind=article -->

- [Holon 的 WorkItem 架构：把工作状态从对话中分离](./why-work-items.md)
  从新版接口上线到旧版下线，解释 WorkItem 与 Goal、Loop 的侧重点：把跨发布周期的目标、进度、外部依赖和交付归入一项持续负责的工作。
  <!-- mdorigin:index kind=article -->

- [不止审一次代码：用 Holon 搭建持续跟进 PR 的 Reviewer](./one-pr-one-work-item.md)
  从模板创建自带工作规范与 Skills 的 reviewer，确认职责与合并权限，订阅仓库 PR，自动审阅并持续跟进修复与 CI。
  <!-- mdorigin:index kind=article -->

- [从个人 AI 工具到团队协作：一个小团队的 Agent Native 实践](./agents-in-a-small-team.md)
  从开发者各自使用 AI，到共享 Agent 参与团队分工：用 77 天的留存用量、设备故障调查和琴键无声的实测案例，记录一个小团队的 Agent Native 实践。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
