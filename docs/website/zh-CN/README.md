---
title: 运行在你自己电脑上的开源 AI 助手
summary: "开源、本地优先的 Rust 常驻 AI Agent（Always-on AI Agent）工作台——提供显式工作项（WorkItem）、事件驱动等待/唤醒与可靠的持续跟进。"
order: 1
---

<div class="home-page home-page--zh">
<section class="home-hero">
<div class="home-hero__copy">

<p class="home-eyebrow">为需要持续跟进的工作而建</p>

# 在你的电脑或<span class="home-nowrap">服务器上，</span>创建并运行常驻 AI 助手。

<p class="home-hero__lede">开源、本地优先的 Rust 常驻 Agent 工作台，面向个人与团队。分配显式工作项（WorkItem）、跨会话保留持久状态，让 Agent 基于事件等待与唤醒持续跟进工作。</p>

<div class="home-actions">
<a class="home-button home-button--primary" href="#install">安装并开始</a>
<a class="home-button home-button--quiet" href="#capabilities">了解工作原理 ↓</a>
</div>

<div class="home-mobile" id="clients" role="group" aria-labelledby="clients-title">
<div class="home-mobile__header">
<h2 id="clients-title">客户端下载</h2>
<span>连接电脑或服务器上的 Holon 服务</span>
</div>
<div class="home-client-pills">
<a class="home-pill" href="https://apps.apple.com/us/app/holon-agent-remote/id6819978897">iOS · App Store ↗</a>
<a class="home-pill" href="https://github.com/holon-run/holon/releases/download/v0.61.1/holon-android-v0.61.1.apk">下载 Android APK ↗</a>
<a class="home-pill" href="https://github.com/holon-run/holon/releases/latest">macOS 菜单栏应用 (.dmg) ↗</a>
</div>
<p class="home-mobile__links"><a href="https://testflight.apple.com/join/XkTfN12S">iOS · TestFlight 测试版 ↗</a> · <a href="/zh-CN/concepts/architecture-comparison">运行时架构对比 →</a> · <a href="/assets/lite-paper/holon-lite-paper-zh-CN.pdf" type="application/pdf" hreflang="zh-CN" aria-label="阅读中文版 Holon Lite Paper（PDF）">产品白皮书 (PDF) ↗</a></p>
</div>

</div>
<div class="home-stage" data-stage-tabs>
<div class="home-stage__bar">
<div class="home-stage-tablist" role="tablist" aria-label="工作台界面切换" hidden>
<button type="button" role="tab" data-stage-tab="web" aria-selected="true">Web 工作台</button>
<button type="button" role="tab" data-stage-tab="tui" aria-selected="false">终端 · TUI 与 CLI</button>
<button type="button" role="tab" data-stage-tab="mobile" aria-selected="false">手机端 · iOS 与 Android</button>
</div>
</div>
<div class="home-stage__panel" data-stage-panel="web">
<a class="home-stage__shot" href="/assets/home-workbench-zh-CN.webp" aria-label="查看 Holon Web 工作台截图大图">
<img src="/assets/home-workbench-zh-CN.webp" width="1600" height="1003" alt="Holon Web 工作台：左侧展示主助手 Holon Bot 与 Leo（代码审阅）、Maya（技术文档）、Nora（内容与增长）四个常驻角色，中间汇总文档与宣发草稿进展，右侧显示 v0.62 发布工作项的计划、阻塞原因与待办清单。" fetchpriority="high" decoding="async">
</a>
</div>
<div class="home-stage__panel" data-stage-panel="tui" hidden>
<div class="home-terminal" role="region" aria-label="在终端通过 CLI 与 TUI 查看 Holon Bot 统筹多角色推进 v0.62 发布">
<div class="home-terminal__chrome" aria-hidden="true"><span class="home-terminal__dots"><i></i><i></i><i></i></span><span class="home-terminal__title">holon — 本地后台服务 · 127.0.0.1:7878</span></div>
<div class="home-terminal__body">
<div class="home-terminal__line"><span class="home-terminal__prompt">$</span> <span class="home-terminal__cmd">holon work-item list</span></div>
<div class="home-terminal__line home-terminal__line--head">WORK ITEM              AGENT          STATUS    TITLE</div>
<div class="home-terminal__line">launch-v062            holonbot       <span class="home-terminal__badge home-terminal__badge--wait">blocked</span>   统筹 v0.62 发布：代码、文档与宣发</div>
<div class="home-terminal__line">demo-leo-work          leo-reviewer   <span class="home-terminal__badge home-terminal__badge--wait">blocked</span>   跟进 PR #128：等待构建与回归测试 CI</div>
<div class="home-terminal__spacer"></div>
<div class="home-terminal__line"><span class="home-terminal__prompt">$</span> <span class="home-terminal__cmd">holon tail --agent holonbot --include-progress</span></div>
<div class="home-terminal__line"><span class="home-terminal__time">10:11</span> <span class="home-terminal__tag home-terminal__tag--ok">maya-docs</span>    已同步快速安装指南与移动端扫码配对文档</div>
<div class="home-terminal__line"><span class="home-terminal__time">10:13</span> <span class="home-terminal__tag home-terminal__tag--ok">nora-growth</span>  已起草 v0.62 发布博客与社媒版本亮点</div>
<div class="home-terminal__line"><span class="home-terminal__time">10:15</span> <span class="home-terminal__tag home-terminal__tag--wait">parked</span>       等待 Leo 回传 PR #128 CI 结果，发布前请你确认</div>
<div class="home-terminal__spacer"></div>
<div class="home-terminal__line"><span class="home-terminal__prompt">$</span> <span class="home-terminal__cmd">holon tui</span></div>
<div class="home-terminal__line home-terminal__line--muted">▸ 已连接本地运行时 · 4 个常驻 Agent 在线 · 2 项已完成，等待 CI 与拍板确认</div>
</div>
</div>
</div>
<div class="home-stage__panel" data-stage-panel="mobile" hidden>
<div class="home-mobile-stage" role="region" aria-label="连接到同一常驻服务的 Holon 手机客户端查看多角色协调进展">
<div class="home-phones" aria-hidden="true">
<div class="home-phone">
<div class="home-phone__notch"><span>9:41</span><i></i><span>5G</span></div>
<div class="home-phone__top"><strong>Holon</strong><span class="home-phone__live">dev-server</span></div>
<div class="home-phone__search">搜索常驻 Agent…</div>
<div class="home-phone__list">
<div class="home-phone__row home-phone__row--active"><span class="home-phone__dot home-phone__dot--wait"></span><div><strong>Holon Bot</strong><small>统筹 v0.62 发布：代码、文档…</small></div><em>等确认</em></div>
<div class="home-phone__row"><span class="home-phone__dot home-phone__dot--wait"></span><div><strong>Leo · 代码审阅</strong><small>跟进 PR #128：等待回归 CI</small></div><em>等 CI</em></div>
<div class="home-phone__row"><span class="home-phone__dot home-phone__dot--ok"></span><div><strong>Maya · 技术文档</strong><small>安装与配对指南已更新</small></div></div>
<div class="home-phone__row"><span class="home-phone__dot home-phone__dot--ok"></span><div><strong>Nora · 内容与增长</strong><small>发布博客与社媒草稿就绪</small></div></div>
</div>
</div>
<div class="home-phone">
<div class="home-phone__notch"><span>9:42</span><i></i><span>5G</span></div>
<div class="home-phone__top"><span>‹ Holon Bot</span><span class="home-phone__chip">文件</span></div>
<div class="home-phone__card">
<span class="home-phone__kicker">V0.62 发布 · 协调简报</span>
<strong>文档与宣发草稿就绪，等待 CI</strong>
<p>Maya 已更新配对文档，Nora 已备好发布文章初稿；仅剩 Leo 跟进的 PR #128 等待 CI。</p>
<div class="home-phone__trace">▸ 已折叠 3 条角色回传与核对记录</div>
</div>
<div class="home-phone__wait">等待 PR #128 CI 与对外发布确认</div>
<div class="home-phone__composer"><span>Leo 确认 CI 通过后即可发布</span><b>↑</b></div>
</div>
</div>
<div class="home-mobile-stage__info">
<span class="home-mobile-stage__kicker">HOLON AGENT REMOTE · IOS &amp; ANDROID</span>
<h3>离开工位，也能随时查看汇总、拍板放行。</h3>
<ul class="home-mobile-stage__list">
<li><strong>扫码一键配对</strong> — 扫描 Web 工作台或 macOS 菜单栏二维码即可连接，可吊销凭据安全存入系统钥匙串。</li>
<li><strong>结果优先时间线</strong> — 直接阅读 Holon Bot 的协调简报或打开 Nora 的草稿文件，底层日志默认折叠不刷屏。</li>
<li><strong>一句话恢复推进</strong> — 在路上确认对外宣发或补充取舍要求，挂起中的工作项收到回复即刻继续。</li>
</ul>
<div class="home-mobile-stage__links">
<a href="https://apps.apple.com/us/app/holon-agent-remote/id6819978897">iOS App Store ↗</a>
<a href="https://github.com/holon-run/holon/releases/download/v0.61.1/holon-android-v0.61.1.apk">下载 Android APK ↗</a>
<a href="/zh-CN/guides/connect-ios-client">iOS 配对指南 →</a>
</div>
</div>
</div>
</div>
<div class="home-stage__callouts">
<div class="home-stage__callout"><span>01 · 多角色协同</span><strong>Holon Bot 统筹专业队友</strong><p>从默认主助手起步，将代码审阅、技术文档、内容与增长交给有名有姓的常驻角色。</p></div>
<div class="home-stage__callout"><span>02 · WORKITEM</span><strong>保存计划与分工清单</strong><p>目标、各角色已完成产出与待交接事项绑定在同一工作项上，跨会话与多天不丢失。</p></div>
<div class="home-stage__callout"><span>03 · 等待与唤醒</span><strong>等 CI、等回传、按需拍板</strong><p>等待外部检查、角色回传或人工确认时安静挂起，条件满足后沿着已有进度原地恢复。</p></div>
</div>
</div>
</section>

<section class="home-section home-start-section" id="install">
<div class="home-start">
<div class="home-start__copy">
<p class="home-kicker">快速开始</p>

## 安装、配置，然后开始。

安装 Holon 后，运行 `holon onboard` 配置模型，再用 `holon daemon start` 启动后台服务。

打开 `http://localhost:7878`，或运行 `holon tui`，创建第一个助手并安排任务。

<p class="home-start__boundary">推荐版本：<a href="https://github.com/holon-run/holon/releases/tag/v0.61.1">v0.61.1</a></p>

<div class="home-actions">
<a class="home-button home-button--primary" href="/zh-CN/getting-started/">完整安装指南 · 各平台</a>
<a class="home-button home-button--quiet" href="https://github.com/holon-run/holon/releases">下载二进制 ↗</a>
</div>

</div>

<div class="home-start__commands" data-install-tabs>
<div class="home-install-tablist" role="tablist" aria-label="安装方式" hidden>
<button type="button" role="tab" data-install-tab="brew" aria-selected="true">macOS · Homebrew</button>
<button type="button" role="tab" data-install-tab="docker" aria-selected="false">Docker Compose</button>
<button type="button" role="tab" data-install-tab="binary" aria-selected="false">Linux 与二进制</button>
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
<p class="home-start__command-label">Docker Compose · 浏览器向导</p>

```bash
git clone https://github.com/holon-run/holon.git && cd holon
docker compose up -d
# 打开 http://localhost:7878/settings 配置模型提供商凭据
```

</div>
<div class="home-install-panel" data-install-panel="binary" hidden>
<p class="home-start__command-label">Linux amd64 / macOS · 预编译二进制</p>

```bash
# 从 https://github.com/holon-run/holon/releases/latest 下载二进制
chmod +x holon && mv holon ~/.local/bin/
holon onboard && holon daemon start
```

</div>
</div>

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

## 一个常驻运行时，<span class="home-nowrap">多端随时接入，</span><span class="home-nowrap">支持团队协作。</span>

在个人电脑或团队 Linux 服务器上运行 Holon。无论通过浏览器、终端（TUI）还是手机连接，看到的都是同一份实时任务进度与上下文。

</div>
<figure class="home-architecture-figure">
<picture class="home-architecture">
<source media="(max-width: 640px)" srcset="/assets/runtime-architecture-zh-narrow.png" width="480" height="550">
<img src="/assets/runtime-architecture-zh.png" width="1200" height="350" alt="Holon 运行时架构：TUI、Web UI 与手机端连接到你的机器或服务器上的 Holon 服务，统一管理常驻 Agents、WorkItems 与等待唤醒机制，并在明确授权的工作区内执行。" loading="lazy" decoding="async">
</picture>
</figure>
<div class="home-team-points">
<div><h3>多端自由切换</h3><p>在终端发起任务，外出时用 iOS 或 Android 查看进展、回复确认，回到电脑在 Web 工作台核对细节。</p></div>
<div><h3>团队共享常驻 Agent</h3><p>部署到团队服务器后，成员可以向同一个 Agent 补充测试反馈或交接问题，沿着已有 WorkItem 继续推进。</p></div>
<div><h3>工作区与权限边界清晰</h3><p>不同项目的文件与产物隔离在各自工作区中，Agent 可访问的目录与工具链完全由你的配置决定。</p></div>
</div>
<p class="home-product__caption">持续跟进需要运行 Holon 的电脑或服务器及后台服务保持开启。</p>
</section>

<!-- TEMPLATES:START -->
<!-- 模板卡片由 agent_templates 与 template-display.json 生成。 -->
<!-- TEMPLATES:END -->

<section class="home-section home-proof">
<div class="home-proof__intro">
<p class="home-kicker">精选文章</p>

## 从理解 Holon，到用它做事。

先动手完成一项任务，再看看团队怎样共享助手，并了解 Holon 的产品思路。

</div>
<div class="home-reading-grid">
<a class="home-reading-card" href="/zh-CN/blog/one-pr-one-work-item">
<img class="home-reading-card__cover" src="/assets/continuous-pr-reviewer-cover.webp" width="1600" height="900" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">01 · 工作流</span>
<h3>不止审一次代码：搭建持续跟进 PR 的 Reviewer</h3>
<p>从 code-reviewer 模板起步，约定职责与权限，让同一个 Reviewer 持续跟进修订与 CI 直至合并。</p>
<span class="home-reading-card__cta">配置持续审阅 →</span>
</a>
<a class="home-reading-card" href="/zh-CN/blog/agents-in-a-small-team">
<img class="home-reading-card__cover" src="/assets/team-shared-agents-cover.webp" width="1536" height="1024" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">02 · 团队协作</span>
<h3>个人有了 AI，团队怎样一起工作？</h3>
<p>从设备故障调查到琴键无声的真机验证，看团队怎样共享 Agent，并根据测试反馈继续调查。</p>
<span class="home-reading-card__cta">看团队如何协作 →</span>
</a>
<a class="home-reading-card" href="/zh-CN/blog/what-is-holon">
<img class="home-reading-card__cover" src="/assets/holon-agent-workspace-cover.webp" width="1600" height="900" alt="" loading="lazy" decoding="async">
<span class="home-reading-card__category">03 · 产品故事</span>
<h3>让多个 Agent 在你的工作环境里持续做事</h3>
<p>从每次叫 AI 帮忙，到让固定角色持续跟进。认识支撑这种协作方式的本地工作台。</p>
<span class="home-reading-card__cta">认识 Holon →</span>
</a>
</div>
<a class="home-reading-all" href="/zh-CN/blog/">查看全部文章 →</a>
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
<a href="https://groups.google.com/g/holon-pioneers" target="_blank" rel="noreferrer">加入 Holon Pioneers 社区 →</a>
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

- [Holon 移动端隐私政策](./privacy.md)
  Holon Android 和 iOS 客户端的数据处理说明。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
