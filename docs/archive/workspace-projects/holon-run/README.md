# holon-run

> Archived from `workspace/projects/holon-run/` during the Holon Rust runtime
> migration. Names that referred to the pre-public runtime incubation line were
> normalized for the public Holon repository.

`projects/holon-run/` 保存 holon-run 相关的 agent 工具、local-first 工作流和工具服务探索材料。

## 基本信息

- `Visibility`: `public`
- `GitHub org`: `https://github.com/holon-run`
- `Local root`: `/Users/jolestar/opensource/src/github.com/holon-run`

## 当前定位

这条线的核心，不是再做一个“AI 应用”，而是从 agent 真正可使用的工具出发，探索：

- local-first 的 agent 工作模式
- agent 可调用的工具服务
- 工具、浏览器、MCP、OpenAPI、CLI 之间的统一接入层
- 面向 agent 的真实执行链路，而不是只停留在聊天层

从当前整体项目组合看，`holon-run` 更像寻找 AI 突破口的主探索线。

## 关键仓库

- `holon`
  - `GitHub repo`: `https://github.com/holon-run/holon`
  - `Local path`: `/Users/jolestar/opensource/src/github.com/holon-run/holon`
- `uxc`
  - `GitHub repo`: `https://github.com/holon-run/uxc`
  - `Local path`: `/Users/jolestar/opensource/src/github.com/holon-run/uxc`
- `webmcp-bridge`
  - `GitHub repo`: `https://github.com/holon-run/webmcp-bridge`
  - `Local path`: `/Users/jolestar/opensource/src/github.com/holon-run/webmcp-bridge`
- `holon-host`
  - `Local path`: `/Users/jolestar/opensource/src/github.com/holon-run/holon-host`
- `agentvm`
  - `Local path`: `/Users/jolestar/opensource/src/github.com/holon-run/agentvm`
- `私有 runtime 线`
  - `Status`: 下一代 `holon` runtime 的迁移源，不作为公开产品边界
- `agentinbox`
  - `GitHub repo`: `https://github.com/holon-run/agentinbox`
  - `Local path`: `/Users/jolestar/opensource/src/github.com/holon-run/agentinbox`

## 当前判断

- 这条线应该继续保留探索强度
- 但要避免同时散成太多独立小实验
- 更适合围绕一个主问题收束：`如何让 agent 在本地优先的环境里，稳定、安全、可组合地使用工具`
- `holon-host` 是当前值得单独观察的新探索方向
- `agent-account-bridge` 已废弃，方向已经并入 `webmcp-bridge`
- `holonbase` 当前暂时放弃，不再作为主线资产继续投入

## 入口文件

- [主线说明（2026）](./strategy/mainline-2026.md)
- [主线与探针模型（2026）](./strategy/mainline-and-probes-2026.md)
- [local-first agent substrate 候选能力图（2026）](./strategy/local-agent-substrate-capability-map-2026.md)
- [holon-host 验证场景矩阵（2026）](./strategy/holon-host-validation-scenarios-2026.md)
- [统一线路 Memo（2026）](./strategy/unified-stack-direction-2026.md)
- [Holon runtime 合并迁移方案（2026-04）](./strategy/holon-runtime-migration-plan-2026-04.md)
- [内部定位 Memo（2026）](./strategy/positioning-memo-2026.md)
- [仓库地图（2026）](./strategy/repo-map-2026.md)
- [优先级与收敛路线图（2026）](./roadmap/focus-2026.md)
- [周计划（2026-03-31 至 2026-04-05）](./roadmap/weekly-plan-2026-03-31.md)

## 建议沉淀内容

- `notes/`：问题记录、实验、评审和跨仓库判断
- `strategy/`：产品主线、能力边界、命名和定位
- `roadmap/`：优先级、主攻方向、收敛计划
- `references/`：外部协议、相关项目、竞品和材料

## 关联项目

- [`../uxc/`](../uxc/)：当前最具体的工具接入层项目
- [`../mdorigin/`](../mdorigin/)：内容工作流侧基础设施
- [`../indexbind/`](../indexbind/)：检索与绑定层基础设施
