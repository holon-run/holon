# pre-public runtime 精简版下一阶段路线图（2026-04-15）

## 背景

当前 `pre-public runtime` 已经不是只停留在 RFC 阶段的 runtime 原型。

过去一轮实际 dogfooding 已经证明：

- 最小可用 TUI 已经落地
- runtime failure surfacing 已经补齐
- provider contract 和 retry policy 已经进入明确、可解释状态
- `serve + tui + daemon` 正在形成完整的本地 operator loop

这意味着下一阶段的规划，不应该默认回到大范围模块扩张，而应该继续围绕真实使用中暴露出来的 friction 做产品收口。

## 对既有模块级建议的判断

原始 `MODULE_LEVEL_PROPOSALS.md` 里有一些方向判断是对的，但主问题是优先级和时机不对。

### 值得保留的部分

- 观察性确实需要增强
- 外部工具集成未来会成为一个方向
- 任务和工作空间相关能力未来还会继续扩

### 当前不适合作为主线的部分

- 先创建大而全的 `observability/` 子系统
- 先标准化完整 `mcp/` 子系统
- 先把 `skills` 做成 catalog / install / template 平台
- 先做多工作空间模板、快照、协调
- 先做 DAG orchestration / queue / template engine

这些方向本身不一定错，但在当前阶段都属于过早平台化。

`pre-public runtime` 现在的优先级，应该继续是：

- 让本地长期运行 runtime 更易启动、更易观察、更易停止
- 让 operator 更容易理解当前系统到底在做什么
- 让真实 dogfooding 更稳定，而不是先铺未来平台地基

## 当前阶段的产品原则

### 1. 优先解决 operator loop friction

任何改动都先问：

- 这是不是实际 dogfooding 中出现的阻塞？
- 它能不能减少 operator 的黑箱感？
- 它能不能让 `run / serve / daemon / tui` 之间的关系更清楚？

如果答案是否定的，就不应该进入主线。

### 2. 优先收口，不优先扩张

现阶段更重要的是：

- 把已经存在的能力做成稳定、可解释、可维护的产品表面

而不是：

- 引入更多抽象层
- 引入更多目录
- 引入更多未来才会用到的子系统

### 3. 观察性要做，但要从最小有效面开始

当前应该补的是：

- operator 能直接看到的状态
- runtime 当前为什么停住
- provider 为什么失败 / retry / fallback
- daemon 当前是否健康

而不是先造一个通用 telemetry 平台。

## 精简版路线图

## Phase 1: Operator Loop 收口

目标：

- 把本地长期运行体验打磨成稳定、清晰、低摩擦的默认使用路径

优先项：

- 继续打磨 `runtime-incubation daemon`
- 继续打磨 `runtime-incubation tui`
- 明确 `run / serve / daemon / tui` 四者的边界和推荐路径
- 收口 daemon status、shutdown、stale state、foreground/background 切换的语义
- 补 operator-facing docs 和 preview usage 文档

完成标准：

- 一个开发者能稳定启动、观察、停止本地 runtime
- 出错时不需要猜系统处于什么状态
- 主要命令的关系不再混乱

## Phase 2: 最小观察性增强

目标：

- 增强调试和解释能力，但不引入重型 observability 子系统

优先项：

- `daemon status` 增加更实用的 runtime 健康信息
- provider retry / fallback / fail-fast 信息进入 operator-facing surface
- task lifecycle 和 child-agent 状态更容易观察
- 为 TUI 增加必要的 runtime diagnostics 视图或状态提示
- 对常见失败生成更结构化、更短、更可操作的摘要

不做的事：

- 不先建 `src/observability/` 大模块
- 不先做事件回放平台
- 不先做 profiling / benchmark 框架化

完成标准：

- operator 能直接看懂最近失败、当前状态、关键路径行为
- 调试一次真实问题时，不需要翻很多内部日志

## Phase 3: 触发式扩展，而非预铺平台

目标：

- 只有在真实使用反复暴露痛点时，才进入下一层抽象

候选方向：

- MCP 集成标准化
- skills 结构扩展
- 更复杂的 workspace 能力
- 更强的 task orchestration

触发条件：

- 同一类外部工具接入问题反复出现
- 本地 skill 管理真的开始成为维护成本
- 多 workspace 协调确实进入真实 operator 需求
- 背景任务依赖关系开始真实复杂到现有模型难以承载

如果没有这些证据，这些方向继续停留在 parking lot。

## Parking Lot

以下方向可以保留，但不进入当前主线：

- 标准化 `src/mcp/`
- skill catalog / install / template / search
- 多工作空间模板、快照、协调
- DAG orchestration / workflow engine
- 大而全的 telemetry / profiling / replay 系统

这些方向应该在“真实需求反复出现后”再启动，而不是现在主动铺开。

## 下一阶段最值得跟踪的信号

接下来判断优先级时，优先看这些真实信号：

- 用户是否经常不知道 runtime 当前是否还活着
- 用户是否经常不知道任务为什么停住
- 用户是否经常需要手动清理本地状态才能恢复
- 用户是否经常需要在 `run / serve / daemon / tui` 之间切换但感到语义混乱
- 用户是否经常需要更清楚地理解 provider retry / fallback 行为

只要这些信号还在，就不该把主线转向大规模平台建设。

## 结论

`pre-public runtime` 当前阶段最重要的，不是扩更多模块，而是继续把它打磨成一个：

- 可持续 dogfood
- 可解释
- 可管理
- 可恢复

的本地长期运行 runtime。

所以当前路线图应该是：

1. 继续收口 operator loop
2. 小步补最小观察性
3. 把平台化扩展放进 parking lot，等真实需求触发
