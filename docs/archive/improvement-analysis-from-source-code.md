# Holon 运行时代码改进分析

## 从用户反馈中发现的改进点

### 问题 1: execution_environment 缺少多 workspace 信息

**现状**：
- 当前 `execution_environment` 只显示**单个活跃 workspace** 的信息
- 虽然 `AgentState` 中保存了 `attached_workspaces: Vec<String>`（所有已加载的 workspace ID 列表）
- 但在构建 prompt 时，这个信息没有被包含到 `execution_environment` section 中

**影响**：
- Agent 不知道自己还加载了其他 workspace
- 无法主动切换 workspace 或查询其他 workspace 的状态
- 用户提问时，Agent 无法准确回答"你知道你加载了几个 workspace 吗？"

**代码位置**：
- `src/context.rs:84` - 构建 `execution_environment` section
- `src/system/host_local_policy.rs:117` - `execution_policy_summary_lines()` 函数
- `src/system/types.rs:168` - `ExecutionSnapshot` 结构定义
- `src/runtime.rs:661` - 构建 `ExecutionSnapshot`

**相关数据结构**：
```rust
// AgentState 中已有这个字段
pub attached_workspaces: Vec<String>,

// ExecutionSnapshot 中缺少这个字段
pub struct ExecutionSnapshot {
    // ... 现有字段
    // 缺少: pub attached_workspaces: Vec<String>,
}
```

### 问题 2: workspace_id 缺少路径映射信息

**现状**：
- `execution_environment` 只显示 workspace ID（如 `ws-51b6165347b14b44a7f554c5e565a191`）
- 没有显示 workspace ID 到路径的映射关系
- Agent 无法知道其他已加载 workspace 的实际路径

**影响**：
- Agent 看到多个 workspace ID，但不知道它们的路径
- 无法智能地选择合适的 workspace 进行操作
- 需要额外的工具调用来查询 workspace 路径

**代码位置**：
- `src/system/host_local_policy.rs:145-150` - 显示 workspace_id 的位置
- `src/host.rs:396` - `workspace_entries()` 方法可以获取所有 workspace 的路径信息

## 改进方案

### 方案 1: 在 ExecutionSnapshot 中添加 attached_workspaces

**修改点 1**: `src/system/types.rs`
```rust
pub struct ExecutionSnapshot {
    pub profile: ExecutionProfile,
    pub policy: ExecutionPolicySnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub workspace_anchor: PathBuf,
    pub execution_root: PathBuf,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_root_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_kind: Option<WorkspaceProjectionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_mode: Option<WorkspaceAccessMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_root: Option<PathBuf>,

    // 新增字段
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attached_workspaces: Vec<WorkspaceInfo>,
}

// 新增结构体
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub workspace_anchor: PathBuf,
    pub is_active: bool,
}
```

**修改点 2**: `src/runtime.rs:661` - 构建 ExecutionSnapshot
```rust
fn execution_snapshot_for_view(
    &self,
    profile: crate::system::ExecutionProfile,
    workspace: &WorkspaceView,
    attached_workspaces: Vec<WorkspaceInfo>, // 新增参数
) -> ExecutionSnapshot {
    ExecutionSnapshot {
        policy: profile.policy_snapshot(),
        profile,
        workspace_id: workspace.workspace_id().map(ToString::to_string),
        workspace_anchor: workspace.workspace_anchor().to_path_buf(),
        execution_root: workspace.execution_root().to_path_buf(),
        cwd: workspace.cwd().to_path_buf(),
        execution_root_id: workspace.execution_root_id().map(ToString::to_string),
        projection_kind: if workspace.worktree_root().is_some() {
            Some(WorkspaceProjectionKind::GitWorktreeRoot)
        } else {
            Some(WorkspaceProjectionKind::CanonicalRoot)
        },
        access_mode: workspace.access_mode(),
        worktree_root: workspace.worktree_root().map(|path| path.to_path_buf()),
        attached_workspaces, // 新增字段
    }
}
```

**修改点 3**: `src/system/host_local_policy.rs:117` - 更新 summary 函数
```rust
pub fn execution_policy_summary_lines(execution: &ExecutionSnapshot) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Backend: {}",
            execution_backend_label(execution.policy.backend)
        ),
        format!(
            "Process execution exposed: {}",
            execution.policy.process_execution_exposed
        ),
        format!(
            "Background tasks supported: {}",
            execution.profile.allow_background_tasks
        ),
        format!(
            "Managed worktrees supported: {}",
            execution.profile.supports_managed_worktrees
        ),
        format!(
            "Projection kind: {}",
            workspace_projection_label(execution.projection_kind)
        ),
        format!(
            "Access mode: {}",
            workspace_access_mode_label(execution.access_mode)
        ),
        format!(
            "Workspace id: {}",
            execution.workspace_id.as_deref().unwrap_or("none")
        ),
        format!("Workspace anchor: {}", execution.workspace_anchor.display()),
        format!("Execution root: {}", execution.execution_root.display()),
        format!("Cwd: {}", execution.cwd.display()),
        format!(
            "Worktree root: {}",
            execution
                .worktree_root
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
    ];

    // 新增：显示所有已加载的 workspace
    if !execution.attached_workspaces.is_empty() {
        lines.push("Loaded workspaces:".into());
        for ws in &execution.attached_workspaces {
            let marker = if ws.is_active { "*" } else { "" };
            lines.push(format!(
                "  {} - {}{}",
                ws.workspace_id,
                ws.workspace_anchor.display(),
                marker
            ));
        }
    }

    lines.extend([
        "Resource authority:".into(),
        format!(
            "  - message_ingress: {}",
            execution_guarantee_label(execution.policy.resource_authority.message_ingress)
        ),
        // ... 其他字段
    ]);

    lines
}
```

### 方案 2: 使用 WorkspaceEntry 列表

另一种方案是直接复用 `WorkspaceEntry` 结构：

```rust
// 使用现有的 WorkspaceEntry 结构
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub workspace_id: String,
    pub workspace_anchor: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// 在 ExecutionSnapshot 中添加
pub struct ExecutionSnapshot {
    // ... 现有字段
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attached_workspaces: Vec<WorkspaceEntry>,
}
```

## 其他发现的改进点

### 改进点 3: AgentState.execution_snapshot 缺少 workspace 信息

**现状**：
- `src/context.rs:2593` 中的测试辅助函数 `execution_snapshot_for()`
- 构建的 ExecutionSnapshot 缺少 `attached_workspaces` 信息

**影响**：
- 单元测试可能无法覆盖多 workspace 场景
- 测试环境的 prompt 构建与生产环境不一致

### 改进点 4: HTTP API 已返回 attached_workspaces

**发现**：
- `src/http.rs:783` - HTTP state API 已经返回 `attached_workspaces`
- 但这个信息没有传递到 prompt 的 execution_environment section

**影响**：
- Web UI 可以看到所有 workspace，但 CLI/TUI agent 不知道
- 不同客户端的体验不一致

## 实施优先级

| 优先级 | 改进点 | 影响 | 工作量 |
|-------|--------|------|--------|
| P0 | 方案 1: 添加 attached_workspaces 到 ExecutionSnapshot | 修复核心功能缺失 | 中 |
| P1 | 方案 2: 使用 WorkspaceEntry 结构 | 更好的复用 | 低 |
| P2 | 改进点 3: 修复测试辅助函数 | 提升测试覆盖率 | 低 |
| P2 | 改进点 4: 统一 HTTP API 和 prompt 的信息 | 提升一致性 | 低 |

## 验证计划

1. **单元测试**：添加测试验证 `execution_policy_summary_lines()` 包含多个 workspace
2. **集成测试**：验证加载多个 workspace 时，prompt 包含完整信息
3. **手动测试**：
   - 加载两个 workspace
   - 提问"你知道你加载了几个 workspace 吗？"
   - 验证 agent 能正确回答并显示所有 workspace 的路径

## 相关代码位置总结

| 文件 | 行号 | 说明 |
|------|------|------|
| `src/system/types.rs` | 168 | `ExecutionSnapshot` 定义 |
| `src/system/host_local_policy.rs` | 117 | `execution_policy_summary_lines()` 函数 |
| `src/runtime.rs` | 661 | 构建 `ExecutionSnapshot` |
| `src/context.rs` | 84 | 构建 `execution_environment` section |
| `src/context.rs` | 2593 | 测试辅助函数 `execution_snapshot_for()` |
| `src/http.rs` | 783 | HTTP state API 返回 `attached_workspaces` |
| `src/tui/render.rs` | 442 | TUI 显示 `attached_workspaces` |
| `src/types.rs` | 871 | `AgentState.attached_workspaces` 字段 |
| `src/host.rs` | 396 | `workspace_entries()` 方法 |

## 参考资料

- 当前 agent state 显示有两个 workspace 加载
- TUI 已经显示 `attached_workspaces`
- HTTP API 已经返回 `attached_workspaces`
- 只有 prompt 的 `execution_environment` section 缺少这个信息
