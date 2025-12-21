# Holon Modes（执行模式）设计

本设计将用户可见的控制面收敛为一个概念：`mode`。`mode` 绑定一组完整的“执行配置”（profile），包含提示词、权限/工具策略、以及期望的输入输出工件，从而避免用户组合出相互冲突的参数（例如“review 但用 coder 的写代码提示词”）。

## 核心概念

### Mode（用户入口）
用户通过 `mode` 表达“这次 Holon 要做什么类型的工作”，例如：
- `execute`：执行/修改代码并输出补丁
- `plan`：只产出计划，不修改代码
- `review`：只产出代码评审意见，不修改代码
- `review-fix`：根据 review comments 修复并更新 PR（可选的复合模式）

### Profile（内部绑定配置）
每个 `mode` 对应一个 profile。profile 由三部分组成：
1) **Prompt Profile**：使用的提示词包/角色（例如 `coder`、`reviewer`、`planner`），以及该模式的“硬约束契约”。  
2) **Execution Semantics**（硬语义）：workspace 挂载权限（RO/RW）、工具许可策略（例如 `permission_mode=plan`）、以及是否允许写文件/运行命令。  
3) **I/O Contract**：该模式要求生成的输出工件及其格式（例如 `diff.patch`、`plan.md`、`review.json`、`summary.md`）。

> 关键原则：**硬语义由 Host 强制**（例如 RO/RW bind mount），而不是“相信提示词”。

## 默认模式与工件契约（建议）

### `mode=execute`（写入/生成补丁）
- Workspace：RW（允许修改 repo）
- 产物（至少）：
  - `/holon/output/diff.patch`
  - `/holon/output/summary.md`
  - `/holon/output/manifest.json`

### `mode=plan`（只读/生成计划）
- Workspace：RO（硬保证不改 repo）
- 产物（至少）：
  - `/holon/output/plan.md`
  - `/holon/output/manifest.json`
- 可选增强：`/holon/output/plan.json`（结构化计划，便于后续自动执行/校验）

### `mode=review`（只读/生成评审）
- Workspace：RO
- 产物（至少）：
  - `/holon/output/review.json`（结构化 findings：`path/line/body/severity/fingerprint`）
  - `/holon/output/summary.md`
  - `/holon/output/manifest.json`

## CLI 与 Spec 建议

### CLI
建议以 `--mode` 作为主要参数：
```bash
holon run --mode execute --spec spec.yaml
holon run --mode plan --goal "..." --workspace . --out holon-output
holon run --mode review --spec spec.yaml
```
说明：
- `--mode` 默认值为 `execute`，保持现有用户体验不变。
- 不对外暴露 `--phase`/`--role` 的任意组合，避免冲突；内部可保留 `phase` 字段用于调试/审计。

### Spec
可在 spec 增加（或未来扩展）：
```yaml
constraints:
  mode: execute  # execute|plan|review|...
```

## 两阶段与复合模式（编排）

有些模式天然需要“多阶段编排”（例如先 plan 再 execute，或先 review 再 fix）。由于 workspace RO/RW 不可在同一容器内动态切换，**多阶段通常意味着两次容器运行**。

建议将其作为“复合 mode”（或单独的 `holon flow` 命令）实现：
- `plan-then-execute`：先 `mode=plan` 产出 `plan.md`，再 `mode=execute` 将 `plan.md` 作为上下文输入并生成 `diff.patch`。
- `review-fix`：先 `mode=review` 产出 `review.json`，再 `mode=execute`（或 `mode=fix`）将 `review.json` 作为上下文输入并更新 PR 分支。

在 GitHub Actions 中，推荐拆成两个 workflow/job，通过 label/comment 作为“人工确认门”。

## Adapter 兼容性

不同 adapter 对“原生 plan/review 能力”支持可能不一致（例如 Claude Agent SDK 支持 `permission_mode="plan"`，但其他 adapter 可能没有）。mode/profile 机制应允许：
- **优先使用原生能力**（如 SDK 的 plan 模式）
- 不支持时使用 **提示词约束 + 工具策略收紧** 做 best-effort
- 但仍依赖 **workspace RO 挂载** 作为最终硬保证

建议在 `manifest.json.metadata` 里记录：
- `mode`
- `workspace_access`（ro/rw）
- `native_plan_used` / `native_review_used`（true/false）

## 与 GitHub 的衔接（发布器）

Holon/adapter 只负责产出标准工件；把结果写回 GitHub（发评论、发 review、更新 PR）由外层（workflow）完成：
- `plan.md` → comment / step summary
- `review.json` → PR review（总评 + 可选 inline）
- `diff.patch` → `git apply` → commit/push → create/update PR

这样 adapter 保持通用，GitHub 逻辑保持可测试、可控且可替换（同一套工件也能用于本地、CI、其他平台）。

