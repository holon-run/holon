# pre-public runtime 并回 holon Memo（2026-04）

日期：`2026-04-06`

这份 memo 记录一轮新的判断：

`pre-public runtime` 是否应该直接替换当前的 `holon`，以及如果要收束，两者应该如何处理。

重点不是讨论实现细节，而是先把：

- 产品名
- repo 角色
- 对外叙事
- 层级边界

这四件事写清楚。

## 一、背景

此前更常见的内部理解是：

- `holon`：assembled product / operator shell / 最终对外入口
- `pre-public runtime`：headless、event-driven、long-lived 的 runtime core

这个切法在更早阶段是合理的。

因为当时 `pre-public runtime` 更像：

- 新的 runtime 探索
- 长时 agent 语义承载层
- 尚未形成完整产品边界的内核

但最近的状态变化已经很明显：

- `pre-public runtime` 已经不只是抽象 runtime
- 它已经有比较完整的 control surface
- 它已经验证了真实的 supervised development loop
- 它在形态上越来越接近此前对 `holon` 的期待

因此，继续长期保留两个并列产品边界，开始变得不自然。

## 二、这次讨论的问题

问题不是：

- `pre-public runtime` 做得对不对

而是：

- 现在是否还应该同时保留 `holon` 和 `pre-public runtime` 两个产品名
- 如果不保留，应该由谁吸收谁

## 三、当前判断

当前更稳的方向是：

- 不再长期保留 `holon` 和 `pre-public runtime` 两个并列产品名
- 保留 `holon` 作为唯一对外产品名
- 把 `pre-public runtime` 视为正在成熟的 runtime / control-plane 实现，并逐步并回 `holon`

一句话：

`不是让 pre-public runtime 取代 holon 这个名字，而是让 pre-public runtime 成为 holon 的现实实现。`

## 四、为什么不是直接把公开名字切成 pre-public runtime

### 1. `holon` 已经是现有公开入口

当前 `holon` 已经承载：

- CLI 名字
- GitHub Action 入口
- Homebrew 安装入口
- `run / solve / serve` 的外部 contract
- 现有文档和对外路径

如果现在反过来让 `pre-public runtime` 成为新的公开主名字：

- 外部入口会被整体打乱
- 迁移成本会明显高于收益
- 还会制造“为什么又换了一个主名字”的额外解释负担

### 2. `pre-public runtime` 的主要价值在实现和方向，不在外部品牌资产

当前 `pre-public runtime` 最强的地方是：

- runtime shape 更清晰
- event-driven / long-lived agent loop 更真实
- control plane 和 wake/sleep 语义更成形

这些价值完全可以被 `holon` 吸收。

不需要为此再制造一个新的公开产品名。

## 五、为什么不能继续长期双名字并行

### 1. 两者开始争夺同一个产品位置

如果继续保留当前结构，外部和内部都会逐步碰到同一个问题：

- `holon` 是不是产品入口
- `pre-public runtime` 是不是也在成为产品入口
- `serve` 的未来到底属于谁

一旦两个名字开始争夺同一位置，认知成本会越来越高。

### 2. `pre-public runtime` 已经长到接近原本对 `holon` 的期待

当前对 `pre-public runtime` 的观察已经不是：

- 一个窄 runtime kernel

而更接近：

- 有控制面
- 有 wake/callback/task/worktree 语义
- 有长期运行 agent 的真实主链路

也就是说，`pre-public runtime` 不再只是“也许以后会被集成”的底层试验。

它已经在逼近 `holon` 原本应该成为的东西。

### 3. 双名字会让文档和仓库地图持续分裂

继续并行意味着后面每次写文档，都要额外解释：

- `holon` 是什么
- `pre-public runtime` 是什么
- 两者边界为什么还没收束

如果这个解释长期存在，本身就说明边界已经不自然了。

## 六、应保留的层级边界

把 `pre-public runtime` 并回 `holon`，不等于把所有层都混成一个名字。

应该继续保留的结构是：

- `holon`：唯一对外产品名，默认入口，runtime + control plane
- `AgentInbox`：ingress / delivery layer
- `uxc`：capability layer
- `webmcp-bridge`：web edge

也就是说，这次收束只发生在：

- `holon`
- `pre-public runtime`

这两个已经开始重叠的产品边界之间。

不应该顺手把其他清晰分层也吞掉。

## 七、推荐迁移方式

### 1. 命名

对外：

- 只讲 `holon`

对内过渡期可以写：

- `Holon is powered by the pre-public runtime runtime`

但 `pre-public runtime` 不应再继续作为并列对外主名字扩张。

### 2. repo 角色

更稳的做法是：

- `holon` 保持 canonical public repo
- `runtime-incubation` 保持迁移期 incubator / runtime lineage repo
- 等收束完成后，再决定 `runtime-incubation` 是否归档或只保留历史

不建议：

- 直接把 `runtime-incubation` 改名为 `holon`
- 再反过来处理今天已经公开存在的 `holon`

这会让发布、安装、入口、历史链接和认知路径都变脏。

### 3. 产品叙事

更顺的一句话应该是：

`holon` 是一个 local-first agent host / runtime product。

内部再拆：

- `holon`：默认入口与控制面
- `AgentInbox`：消息、事件、投递和唤醒接入
- `uxc`：统一能力接入
- `webmcp-bridge`：浏览器和网页登录态边缘接入

### 4. 技术迁移顺序

推荐顺序：

1. 先统一文档和命名判断
2. 把 `holon serve` 的未来明确绑定到 `pre-public runtime` 方向
3. 优先让 `pre-public runtime` 的 runtime 能力成为 `holon serve` 的实现主线
4. 再评估 `holon run` 是否逐步与同一 substrate 收口
5. `holon solve` 继续作为更高层 workflow wrapper，不急着先动

## 八、不推荐的路线

### 1. 长期保持双主名字

不推荐让 `holon` 和 `pre-public runtime` 长期并列存在，并分别讲：

- 一个是未来产品
- 一个是现实 runtime

这条路在早期可行，但随着 `pre-public runtime` 成熟，会越来越别扭。

### 2. 让 `pre-public runtime` 直接成为新的公开主品牌

不推荐为了实现方向更先进，就把公开入口整体切到 `pre-public runtime`。

这会重置已有产品资产，却不一定带来同等收益。

### 3. 顺势把其他层都收进 `holon`

不推荐把这次收束误解成：

- `AgentInbox` 也该消失
- `uxc` 也该消失
- `webmcp-bridge` 也该消失

这几层仍然有清晰边界，应继续独立存在。

## 九、建议的下一步

### 1. 出一份正式并回 RFC

至少明确：

- 命名映射
- CLI 映射
- repo 角色
- `serve / run / solve` 的未来关系
- 状态目录和 API 兼容面

### 2. 先改文档，再动 repo 名

在真正做仓库层动作前，先把：

- `workspace/projects/holon-run`
- `holon`
- `runtime-incubation`

这三处的角色关系写一致。

### 3. 先完成 `serve` 方向的收束

如果要验证这条并回路线，最应该先验证的是：

- `holon serve` 是否能够自然吸收 `pre-public runtime` 的 runtime 形态

这是最核心的重叠区。

## 十、一句话判断

`holon` 这个名字应该保留。

`pre-public runtime` 这条实现路线也应该保留。

更合理的收束方式不是二选一，而是：

`让 pre-public runtime 并回 holon，让 holon 成为唯一公开产品边界。`
