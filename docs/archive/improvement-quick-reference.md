# Holon 改进点速查表

## 从源码分析中发现的关键改进点

### 🔴 P0: execution_environment 缺少多 workspace 信息

**问题**：
- Agent 知道加载了多个 workspace（`attached_workspaces: Vec<String>`）
- 但 prompt 的 `execution_environment` section 只显示活跃 workspace
- 导致 Agent 无法回答"你知道你加载了几个 workspace吗？"

**影响**：
- 用户提问时，Agent 需要调用工具才能知道其他 workspace 的路径
- 不同客户端（HTTP API vs CLI prompt）的信息展示不一致
- TUI 已显示 `attached_workspaces`，但 prompt 中缺失

**解决方案**：
1. 在 `ExecutionSnapshot` 中添加 `attached_workspaces` 字段
2. 更新 `execution_policy_summary_lines()` 显示所有 workspace
3. 修改 `execution_snapshot_for_view()` 传递 workspace 列表

**文件清单**：
- `src/system/types.rs:168` - 添加字段到 `ExecutionSnapshot`
- `src/system/host_local_policy.rs:117` - 更新 `execution_policy_summary_lines()`
- `src/runtime.rs:661` - 修改构建逻辑
- `src/context.rs:84` - 使用新的字段

## 其他发现的改进点

### 🟡 P1: 测试辅助函数缺少 workspace 信息

**位置**：`src/context.rs:2593`
**问题**：`execution_snapshot_for()` 构建的 snapshot 缺少 `attached_workspaces`
**影响**：单元测试无法覆盖多 workspace 场景

### 🟢 P2: HTTP API 和 prompt 信息不一致

**位置**：
- `src/http.rs:783` - HTTP API 已返回 `attached_workspaces`
- `src/context.rs:84` - prompt 的 execution_environment 缺少此信息

**影响**：Web UI 可以看到，但 CLI agent 不知道

## 验证步骤

### 1. 复现问题
```bash
# 加载两个 workspace
holon workspace attach /path/to/workspace1
holon workspace attach /path/to/workspace2

# 提问 agent
echo "你知道你加载了几个 workspace 吗？" | holon prompt

# 当前结果：agent 回答错误或需要调用工具
# 预期结果：agent 能直接回答并显示所有 workspace
```

### 2. 验证修复
```bash
# 运行单元测试
cargo test execution_policy_summary_lines

# 运行集成测试
cargo test build_context

# 手动测试
holon prompt "列出所有已加载的 workspace"
```

## 实施检查清单

- [ ] 在 `src/system/types.rs` 中定义 `WorkspaceInfo` 结构
- [ ] 在 `ExecutionSnapshot` 中添加 `attached_workspaces: Vec<WorkspaceInfo>`
- [ ] 更新 `execution_snapshot_for_view()` 传递 workspace 列表
- [ ] 修改 `execution_policy_summary_lines()` 显示所有 workspace
- [ ] 更新 `src/context.rs:2593` 的测试辅助函数
- [ ] 添加单元测试
- [ ] 添加集成测试
- [ ] 更新文档

## 相关 Issue

建议创建新 issue：
- 标题：`prompt: execution_environment should include all attached workspaces`
- 优先级：P0
- 标签：`improvement`, `prompt`, `workspace`
