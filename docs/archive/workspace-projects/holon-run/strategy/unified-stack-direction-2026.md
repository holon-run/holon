# holon-run 统一线路 Memo（2026）

这份文档讨论的不是某个单独 repo。

它讨论的是一个更现实的问题：

`holon`、`holon-host`、`uxc`、`webmcp-bridge` 这几条线，未来应该怎样收成一条统一线路。

当前的问题不是方向冲突。

而是：

- 每条线各自都对
- 但放在一起时，整体叙事还不够顺
- 外部人很容易看到几个并列名字，而不是一套连续结构

## 一、结论先行

当前最合理的理解，不是让 `holon-host` 长期作为一个独立主名字存在。

更合理的方向是：

- `holon-host` 继续作为内部探索名
- 它验证的是 `holon` 下一步真正该长出来的那部分
- 如果验证成立，相关能力应该逐步并回 `holon`

也就是说：

`holon-host` 更像是 `holon` 的第二幕探针，而不是长期并列产品名。`

## 二、当前真正的割裂点是什么

当前的割裂，不只是 repo 多。

更核心的是，这几条线分别在讲不同层：

- `uxc` 在讲 capability access
- `webmcp-bridge` 在讲 web edge
- `holon` 在讲 runtime / workflow shell
- `holon-host` 在讲 app -> host control plane

每一层都成立。

但如果没有一条更上位的统一结构，结果就会像：

- 都重要
- 但谁是中心不够明确
- 哪些应该并入，哪些应该独立，也不够明确

## 三、一个更顺的统一结构

当前我更倾向于把这几条线理解成下面三层。

### 1. `uxc`

角色：

- capability plane
- 协议与工具接入层
- 调用、订阅、连接 contract

它解决的是：

`agent 和 host 侧系统，怎样碰到真实能力。`

### 2. `webmcp-bridge`

角色：

- web edge
- 浏览器能力和网页登录态接入层
- WebMCP native 路径和 fallback adapter 路径

它解决的是：

`浏览器和 web app 这一块，怎样进入同一条能力接入链。`

### 3. `holon`

角色：

- host substrate
- local-first control plane
- 轻量运行环境、通知入口、session binding、artifact/control surface

它解决的是：

`agent 在用户侧持续工作时，谁来承接通知、控制面和最小运行环境。`

## 四、为什么 `holon-host` 很像应该并回 `holon`

`holon-host` 当前在验证的，不是一个全新的平行方向。

它真正碰到的问题是：

- app 希望发 attention request
- 现有 agent 多数不能自然接受外部 app 通知
- 用户仍然希望继续使用自己已有的主 agent
- 中间需要一层轻量 host substrate，把通知、绑定、回写、控制面接住

这件事，本质上不是“另一个 host 产品”。

它更像：

`holon` 作为 host substrate，原来还没长出来的那部分能力。`

也就是说，`holon-host` 不是在证明需要一个新的大名字。

它是在证明：

`holon` 不应该只被理解成 runtime 或 workflow shell，它还应该拥有 host control plane 这一层。`

## 五、为什么不能直接让现有 agent 当平台

当前一个很现实的限制是：

- Codex
- Claude Code
- 以及其他现有 agent

它们都更像：

- intelligence engine
- task runtime
- 用户主动驱动的 agent 界面

而不是天然适合作为：

- app notification receiver
- host-side inbox
- user-side control plane
- local substrate for multiple protocol-native apps

所以不能简单假设：

`以后 app 直接通知 Codex / Claude Code 就够了。`

在现阶段，更合理的结构是：

- 现有 agent 继续承担 intelligence
- `holon` 提供轻量 substrate
- app 通过这层 substrate 与外部 agent 形成闭环

## 六、这意味着 `holon` 应该长成什么

如果按这个方向继续收，`holon` 更适合长成下面这些东西：

### 应该拥有的

- sandbox / workspace runtime
- notification intake
- inbox / attention queue
- app session to host session binding
- artifact / result handling
- visible control surface
- host adapter interface

### 不应该拥有的

- 自己重新做一套大模型平台
- 自己吞掉 `uxc`
- 自己变成大而全 chat-first assistant
- 自己承担全部 intelligence

一句话：

`holon` 应该拥有控制面，不应该拥有全部智能。`

## 七、`holon-host` 这个名字怎么处理

当前不建议现在就直接重命名。

更稳的做法是：

### 短期

- `holon-host` 继续作为内部 probe 名
- 专门验证 app -> host 这一层是否真实存在

### 中期

- 如果第二个、第三个验证场景成立
- 把 `holon-host` 的能力并回 `holon` 叙事
- 对外减少 `holon-host` 这个并列名字

### 长期

- `holon` 成为统一 host substrate 名
- `uxc` 保留为 capability layer
- `webmcp-bridge` 保留为 web connector / edge

## 八、统一后的一句话结构

如果未来要把这条线讲顺，我当前更倾向于：

`holon-run` 在做一套 local-first agent host substrate。`

内部再拆成：

- `holon`：host substrate / control plane
- `uxc`：capability layer
- `webmcp-bridge`：web edge

这个说法比“很多并列 repo”更连续，也比单独讲 connectivity 更完整。

## 九、为什么现在还不能立刻改成这个最终说法

因为 `holon-host` 仍然在 probe 阶段。

当前还没有足够证据证明：

- app -> host attention 模式是跨场景成立的
- host control plane 真比直接让 agent 调工具更自然
- `holon` 真的应该吸收这部分能力

所以现在更合适的姿势是：

- 在内部先按这条逻辑理解
- 在外部先保持克制
- 等验证信号更强时，再正式统一命名和叙事

## 十、一句话判断

`holon-host` 值得继续验证。`

但它更像是 `holon` 下一步该长出来的控制面能力，而不是长期要与 `holon` 并列存在的独立主名字。
