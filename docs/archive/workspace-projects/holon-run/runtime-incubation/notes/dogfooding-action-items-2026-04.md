# pre-public runtime Dogfooding Action Items

Date: 2026-04-06
Derived from: `notes/dogfooding-retrospective-2026-04.md`

## Goal

把这轮 dogfooding 的复盘收敛成少量、可执行、按优先级排序的产品改进项。

## Priority 1: Tighten the verification loop

### Why

这轮最稳定的失败模式不是实现，而是验证：

- 测试已经通过，但 agent 还会继续追加验证命令
- 输出不够理想时，agent 会自己发明新的 shell 命令
- 长命令虽然已经有 `TaskOutput`，但成功收口还不够稳

### Desired outcome

让一次明确的验证动作在通过后自然终止，而不是拖成新的探索回合。

### Suggested scope

- 改进成功 `TaskOutput` / `TaskResult` 后的后续处理
- 减少“测试已通过但仍继续验证”的概率
- 保持 `TaskOutput` 为主路径，不退回 shell workaround

## Priority 2: Harden constrained repair mode

### Why

review-fix 比第一次实现更容易跑偏：

- 任务本来很窄
- agent 先局部修复
- 然后重新扩 scope 或追加额外命令

### Desired outcome

把“只修这个 review 点”变成更稳定的执行模式。

### Suggested scope

- 继续改进 constrained repair prompt
- 强化 acceptance check 的保真度
- 降低修复后重新打开 scope 的概率

## Priority 3: Enforce managed worktree for supervised coding flows

### Why

当前 worktree 已经能用，但还主要依赖：

- prompt
- staged supervision
- 操作习惯

它还不是 runtime 强约束。

### Desired outcome

对于受监督的开发任务，未进入 managed worktree 时不能直接编辑。

### Suggested scope

- 为特定 operator task 增加 “requires managed worktree” 模式
- mutating tools 在不满足条件时直接拒绝

## Priority 4: Improve long-lived context preservation

### Why

虽然 `current_input` 截断问题已经修了，但长期运行 agent 的上下文仍然会越来越难驾驭：

- 长 follow-up 更容易漂
- 最有效的监督方式仍然是 “一个文件 + 一个命令 + 一个成功条件”

### Desired outcome

让长生命周期 agent 在 review-fix 阶段仍然能保留高保真 operator 指令。

### Suggested scope

- 改进 compaction 策略
- 保留 operator prompt 的 tail fidelity
- 降低上下文膨胀后对 follow-up 的侵蚀

## Priority 5: Extend AgentInbox GitHub support to CI-oriented events

### Why

真实 GitHub review/comment wake 已经跑通，但 CI 事件还没有接进来。

这不是 `pre-public runtime` 主体的缺陷，而是当前 `AgentInbox` GitHub source 的覆盖范围还不够。

### Desired outcome

把真实 CI 信号也接进同一条 AgentInbox -> pre-public runtime continuation loop。

### Suggested scope

- 扩 GitHub source beyond repo activity feed
- 增加 `check_run` / `check_suite` 或同等 CI-oriented event 支持
- 相关跟踪见 `holon-run/agentinbox#2`

## Recommended execution order

1. Tighten the verification loop
2. Harden constrained repair mode
3. Enforce managed worktree for supervised coding flows
4. Improve long-lived context preservation
5. Extend AgentInbox GitHub support to CI-oriented events

## Operating note

在这些改进落地之前，下一轮 dogfooding 仍建议继续使用当前最稳的监督协议：

- 一个新 agent 对应一个实质性任务
- 一个 task 一个 worktree
- follow-up prompt 保持单目标
- 尽量给唯一验证命令
- 连续 repair drift 后及时接管
