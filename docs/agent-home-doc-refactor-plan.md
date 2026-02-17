# Agent Home 文档重构执行方案

## 背景

Holon 已从“单次执行工具”演进为“以 `agent_home` 为核心的运行时”：
- `holon run` 是稳定执行内核
- `holon solve` 是 `run` 的 GitHub 场景封装
- `holon serve` 是长期运行的主动式 agent（实验性）

现有 `README.md`、`AGENTS.md`、`rfc/` 仍有旧叙事与旧路径假设，需统一。

## 目标

1. 对外入口文档与当前实现一致。
2. 去除技能文档中的 Holon 路径耦合表达。
3. 为 RFC 建立可维护状态机制，明确哪些内容代表当前实现，哪些是草案或已被替代。

## 执行范围

- `README.md`
- `AGENTS.md`
- 新增 `docs/architecture-current.md`
- 新增 `rfc/README.md`
- 更新 `rfc/0001` ~ `rfc/0006` 的状态元信息与实现说明

## 执行步骤

1. 重写 `README.md` 顶层信息架构  
   - 明确 `run/solve/serve` 定位  
   - 增加 `agent_home` 模型说明  
   - 将“稳定能力”与“实验能力”分层
2. 更新 `AGENTS.md` 约束  
   - 明确 skill 不应硬编码 Holon 私有路径  
   - 路径语义变更必须同步 README/RFC/docs
3. 建立“当前架构基线”文档  
   - 说明运行时边界、输入输出契约、agent_home 作用
4. 建立 RFC 状态索引  
   - 定义 `Active` / `Draft` / `Superseded` / `Deprecated`
5. 清理 RFC 元信息  
   - 为每个 RFC 增加实现状态提示（Implementation Reality）

## 验收标准

1. 新人仅阅读 `README.md` 可理解当前三层模型和稳定边界。
2. skill 作者从 `AGENTS.md` 可明确路径耦合禁忌。
3. `rfc/README.md` 能快速判断每篇 RFC 的“当前有效性”。
4. RFC 文档不再将过期约束误导为当前事实。

## 本次改动说明

- 本文档与代码库文档同步提交，作为本轮重构的执行记录。
