# holon-host 验证场景矩阵（2026）

这份文档不是要给 `holon-host` 找更多 demo。

它要回答的是：

`如果 holon-host 不是 board 特例，而是一类真实产品模式，那么第二个、第三个验证场景应该选什么。`

## 一、先说结论

当前我不建议继续找更多“看起来也能接 agent 的 app”。

这会让方向再次散掉。

更合理的方式是：

- 先保留 `board` 作为第一个 reference case
- 再选两个差异足够大的 app 类型
- 用它们验证 `app -> host agent -> structured writeback` 这个模式是否跨类别成立

当前我更推荐的验证顺序是：

1. 文档 / 演示稿编辑器
2. 交互式 code review / 代码审阅面
3. 棋盘 / 对局类 app 作为补充验证，不优先

## 二、怎么判断一个场景值不值得拿来验证

不是看它能不能调用 LLM。

而是看它是否同时满足下面四条：

1. app 自己已经有明确状态和交互面
2. 用户会在 app 内自然产生“这里需要智能帮助”的时刻
3. app 本身不值得内嵌一个完整 agent loop
4. 结果可以结构化回写到 app，而不是只吐一段聊天文本

如果四条里只满足一两条，这种 app 更像普通 AI 功能，不像 `holon-host` 要验证的模式。

## 三、当前模式假设

`holon-host` 真正要验证的，不是 “app 能接 host agent” 这么宽泛的话。

更准确一点，是下面这个模式：

- app 持有状态
- host 持有智能
- 用户已有自己的主 agent
- app 在局部上下文里发起 attention request
- host 返回结构化结果
- app 把结果写回自己的 surface

这个模式如果在不同类别里都成立，`holon-host` 才有成为第二幕的资格。

## 四、候选场景矩阵

## A. 文档 / 演示稿编辑器

例子：

- Markdown 编辑器
- 幻灯片编辑器
- block editor

### 为什么这个场景强

- 状态明确，文档、选区、block、cursor 都是天然上下文
- 用户会频繁出现局部智能需求
  - 扩写
  - 改写
  - 重组结构
  - 生成图示
  - 调整页结构
- 回写路径天然存在
  - 插入段落
  - 修改 block
  - 更新 slide 结构

### 它验证什么

- `holon-host` 是否适用于高频通用生产力场景
- attention request 是否能由选区、cursor、block 自然触发
- structured writeback 是否比 chat copy/paste 更值钱
- 最终用户是否需要 visible control surface

### 它的风险

- 很容易直接滑成“做一个 AI 编辑器”
- 如果产品壳太重，会掩盖 `holon-host` 真正在验证的 host control plane 价值

### 结论

这是当前最值得作为第二个验证场景的类型。

## B. 交互式 code review / 审查面

例子：

- PR review surface
- code comment triage
- 结构化 review workbench

### 为什么这个场景强

- 你自己就有强烈 dogfooding 条件
- 代码 review 天然是局部上下文驱动
  - diff
  - comments
  - files
  - suggested fix
- 用户很可能已经有主 agent，不想每个 review 工具再嵌一套完整 agent

### 它验证什么

- 开发者工作流里，app -> host 的模式是否真比直接让 agent 调工具更自然
- host session continuity 是否对审查面有价值
- structured draft / apply 模式是否比聊天式建议更有效

### 它的风险

- 太容易和 `holon` 自己的 coding / issue-to-pr 叙事搅在一起
- 如果做得不克制，会看起来像“又一个 coding agent shell”

### 结论

这是当前最值得作为第三个验证场景的类型。

但它更适合作为第二波，而不是第一波，因为它和现有主线认知距离更近，容易把边界搅混。

## C. 棋盘 / 对局 / 白板类 app

例子：

- 国际象棋 / 围棋分析板
- 可交互图解板
- board 类 visual thinking app

### 为什么这个场景有价值

- 状态可视化强
- 选区和局面天然结构化
- 很容易做出漂亮 demo
- app 状态和 host 智能分离得很清楚

### 它验证什么

- 结构化状态 + 结构化回写 的闭环是否顺
- visual surface 是否天然适合 host intelligence 回流
- 浏览器协作面是否是高价值入口

### 它的风险

- 很容易停在 “很好看但需求不够大” 的 demo
- 商业和日常生产力说服力偏弱

### 结论

它适合做第一个 reference case。

但不适合单独作为 `holon-host` 的主要市场证明。

## 五、为什么我不把棋盘类放在优先级第一

不是因为它不好。

恰恰相反，`board` 作为第一个切口很对。

问题在于，如果继续只围绕它打转，外部人很容易把 `holon-host` 理解成：

- 给可视化工具加 AI
- 一个 board-specific agent loop
- 或者 `webmcp-bridge` 的附属 demo

文档 / 演示稿编辑器和 code review 工具，能更好证明这不是 board 特例。

## 六、推荐验证顺序

### 第一阶段

保留 `board`，不再扩大它的产品范围。

目标：

- 把第一个闭环做扎实
- 证明 attention inbox + host adapter + writeback 这条链是可行的

### 第二阶段

找一个文档或演示稿编辑器场景。

目标：

- 验证高频生产力场景里，`app -> host` 是否成立
- 验证最终用户是否更需要一个 visible control surface

### 第三阶段

找一个 code review / 审查面场景。

目标：

- 验证 developer workflow 里，这种模式是否比现有 `agent -> tool` 更自然
- 验证 host session continuity 是否有真实价值

## 七、验证姿势

当前不建议为了验证 `holon-host`，自己重做一个完整的文档编辑器、演示稿编辑器或 code review 工具。

这会把验证问题带偏。

更合适的姿势是：

- 先用轻量 reference demo 验证模式
- 先证明 `app -> host -> structured writeback` 这条链能跑通
- 尽量把精力放在 host control plane、attention envelope 和最小 app-side adapter
- 如果模式成立，再去接已有开源项目，而不是自己重造产品壳

一句话：

`先验证控制面，不先重造应用面。`

## 八、每个场景真正要测的，不是“能不能接”

每个验证场景，至少要回答下面这些问题：

1. 用户为什么不想让 app 自己内嵌完整 agent
2. 为什么不能只让外部 agent 直接 call app
3. app 内部哪个时刻最自然地产生 attention request
4. host 返回什么样的结构化结果才真正有价值
5. 回写后，用户是在 app 内完成任务，还是又跳回聊天框

如果最后用户还是主要在聊天框里完成任务，那 `holon-host` 的产品意义就弱了。

## 九、当前最推荐的实际组合

如果只选两个下一步验证场景，我当前建议：

### 组合 A

- `board`
- `markdown / slide editor`

好处：

- 一个 visual app
- 一个 text-first productivity app
- 差异足够大

### 组合 B

- `board`
- `interactive code review surface`

好处：

- 更贴近你自己的工作流和生态
- 更容易 dogfood

风险：

- 容易和 `holon` 主线边界混淆

所以我当前更倾向于：

`先做 board + 文档/演示稿编辑器，再看是否需要补 code review。`

## 十、一句话判断

`holon-host` 接下来最重要的，不是接更多 app。

而是选两个差异足够大的 app 类型，验证它所代表的是一类共同模式：

`app 持有状态，host 持有智能，attention 从 app 发起，结构化结果再写回 app。`
