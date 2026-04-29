# holon-run 周计划（2026-03-31 至 2026-04-05）

这份周计划不追求面面俱到。

当前阶段更重要的是：

- 把 `holon-host` 作为高价值 probe 往前推一步
- 把统一线路再收紧一轮
- 不再扩张新的并列实验

## 本周目标

### P0

把 `holon-host` 的最小闭环打稳，证明这条 probe 值得继续。

### P1

把 `holon / holon-host / uxc / webmcp-bridge` 的边界再收紧一轮。

### P2

筛出下一步可能接入的开源 app 候选，但本周不接入、不重造产品壳。

### P3

准备一条更清楚的对外表达，让外部逐步理解这条线不只是 connectivity。

## Tue 2026-03-31

### 任务

- 修 `holon-host` 当前最小闭环中的明显问题
- 先把 `board -> attention -> host -> draft -> apply` 这条链收成唯一演示路径
- 把当前 demo 的卡点列出来

### 当天产出

- 一条可重复跑的 demo 步骤
- 一份简短卡点列表

## Wed 2026-04-01

### 任务

- 集中补 `holon-host` vertical slice 的稳定性
- 不扩功能面，只补影响演示可靠性的地方
- 让 inbox / binding / host adapter / writeback 这条链更稳

### 当天产出

- 一个可演示版本
- 一份最小 demo note 或 README 补充

## Thu 2026-04-02

### 任务

- 做边界收敛
- 只回答下面四个问题：
  - 哪些能力未来属于 `holon`
  - 哪些继续属于 `uxc`
  - 哪些继续属于 `webmcp-bridge`
  - `holon-host` 哪些只是 probe 临时形态

### 当天产出

- 一版可复用的边界判断
- 不追求新大文档，优先收敛现有 memo

## Fri 2026-04-03

### 任务

- 做下一步候选开源项目筛选
- 分类只看两类：
  - 文档 / 演示稿编辑器
  - 交互式 code review / 审查面
- 每类只保留 `2-3` 个候选

### 筛选标准

- 有没有明确状态面
- 有没有插件或扩展入口
- 能不能结构化回写
- 接入难度是否可控

### 当天产出

- 一个 shortlist
- 本周不联系、不实现、不新开 repo

## Sat 2026-04-04

### 任务

- 做一次轻复盘
- 判断 `holon-host` 这周是否把 “host control plane probe” 往前推了一步
- 整理一条可对外表达的判断

### 更适合准备的表达

- X 长帖
- 或 blog 短文

主题方向：

`为什么 local-first agent 不只需要 connectivity，还需要 host-side control plane`

## Sun 2026-04-05

### 任务

- 收尾本周结论
- 判断下周是继续单 probe，还是开始接触 shortlist 里的候选项目
- 把下周任务压回 `1 主线 + 1 probe`

## 本周验收标准

- `holon-host` demo 能稳定讲清楚，不只是概念
- 能用一句话解释 `holon` 和 `holon-host` 的关系
- 有一份下一步候选 app shortlist
- 有至少一条可对外发出的方向判断

## 本周不做

- 不自己做完整编辑器或 code review 工具
- 不新增并列 repo
- 不同时推进多个 probe
- 不把 `holon-host` 过早包装成正式产品线
