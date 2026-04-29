# holon-run 仓库地图（2026）

这份文档的目的不是罗列所有仓库，而是把当前 `holon-run` 目录里的仓库按角色分层，方便后续做聚焦和清理。

## 总体判断

当前 `holon-run` 下的仓库已经明显分成四类：

1. 主仓库
2. 辅助仓库
3. 实验 / issue 仓库
4. worktree / 临时工作目录

真正应该长期维持清晰定位的，只是前两类。

## 一、主仓库

这些仓库直接承载主线。

### `holon`

- 本地路径：`/Users/jolestar/opensource/src/github.com/holon-run/holon`
- 当前角色：主产品入口 / agent runtime / solve workflow
- 为什么是主仓库：
  - 它已经是最完整的产品入口
  - 有清晰的 CLI、runtime、skills、GitHub workflow 叙事
  - 最接近“外部人如何真正使用这套东西”

### `uxc`

- 本地路径：`/Users/jolestar/opensource/src/github.com/holon-run/uxc`
- 当前角色：统一工具接入层
- 为什么是主仓库：
  - 它最直接承载“跨协议统一调用”这个核心问题
  - 对 `holon-run` 的长期价值非常基础
  - 很多其他方向都应该建立在它之上，而不是绕开它

## 二、辅助仓库

这些仓库重要，但更适合被视为主线的支撑层，而不是并列主产品。

### `agentinbox`

- 本地路径：`/Users/jolestar/opensource/src/github.com/holon-run/agentinbox`
- GitHub repo：`https://github.com/holon-run/agentinbox`
- 当前角色：local agent ingress / inbox / delivery layer
- 与主线关系：
  - 它负责把外部系统的消息与事件接到本地 agent
  - 应作为 `runtime-incubation`、`uxc` 与外部 source/delivery 之间的共享接入层
  - 更适合作为辅助基础设施层，而不是单独对外主产品

### `webmcp-bridge`

- 本地路径：`/Users/jolestar/opensource/src/github.com/holon-run/webmcp-bridge`
- 当前角色：浏览器 WebMCP 与本地 MCP 之间的桥接层
- 与主线关系：
  - 它强化了浏览器侧工具接入能力
  - 应服务于 `holon` 和 `uxc` 的整体故事
  - 不宜变成一条完全独立的对外主叙事

补充判断：

- 原来的 `agent-account-bridge` 方向已经并入这里
- 后续不再把两者视为两条独立产品线

### `holon-host`

- 本地路径：`/Users/jolestar/opensource/src/github.com/holon-run/holon-host`
- 当前角色：host control plane / app-host 关系探索
- 与主线关系：
  - 这是一个新的探索方向
  - 值得继续观察，但现阶段还不应抢走主线入口

### `agentvm`

- 本地路径：`/Users/jolestar/opensource/src/github.com/holon-run/agentvm`
- 当前角色：长时运行 agent 的执行语义与 runtime 抽象
- 与主线关系：
  - 它更像底层执行模型研究
  - 适合作为长期技术资产
  - 但不应直接成为当前对外主叙事中心

## 三、实验 / issue 仓库

这类仓库的特点是：

- 服务单一问题
- 服务单一实验
- 服务单一集成
- 价值主要在验证，不在长期承载

当前看到的典型例子：

- `holon-host`
- `uxc-issue-328`
- `uxc-local-schema-file`
- `uxc_discord_gateway`
- `uxc_feishu_subscribe`
- `uxc_idle_ttl`
- `uxc_issue272`
- `uxc_stdio_exit_fix`
- `uxc_sui_jsonrpc_skill`
- `uxc_webmcp_file_paths`
- `webmcp-bridge_google`
- `webmcp-bridge_grok_files`
- `webmcp-bridge_session_recovery`
- `holon-test`
- `holon-one`
- `agent-account-bridge`
- `holonbase`

这里面有些内容未来会升级成正式能力，但在升级之前，都应该被视为实验资产，而不是主资产。

其中：

- `agent-account-bridge`：已废弃，方向并入 `webmcp-bridge`
- `holonbase`：当前暂时放弃，更适合视为待归档资产
- `holon-host`：虽然是新方向，但现阶段仍按探索资产管理

## 四、worktree / 临时工作目录

这类目录默认不应该被纳入长期项目结构叙事。

当前包括：

- `holon_worktrees`
- `uxc_worktree`
- `uxc_worktree2`
- `webmcp-bridge-worktrees`

这些目录主要是开发过程资产，不是产品资产。

## 五、建议的管理动作

### 1. 明确长期保留名单

建议先把长期核心名单收敛成：

- `holon`
- `uxc`
- `webmcp-bridge`
- `holon-host`
- `agentvm`

### 2. 给实验仓库加一层显式标签

可以在 `workspace/projects/holon-run` 视角里明确标记：

- `core`
- `support`
- `experiment`
- `worktree`

这样以后讨论时不需要再重新判断。

### 3. 定期做一次实验仓库清理

实验仓库如果长期不回收，会制造两个问题：

- 仓库名越来越像正式产品线
- 精力被很多半活跃方向持续分散

建议后面定期判断：

- 合回主仓库
- 升级为辅助仓库
- 归档
- 删除本地工作目录

## 六、当前最值得保留的结构

从今天看，一个更健康的结构是：

- `holon`：主产品入口
- `uxc`：主能力接入层
- `webmcp-bridge`：浏览器桥接层
- `holon-host`：新探索方向
- `agentvm`：专题技术支撑层
- 其他 `uxc_*` / `webmcp-bridge_*` / worktrees：实验层
- `holonbase` / `agent-account-bridge`：停止推进或待归档资产

## 七、一句话判断

`holon-run` 当前的问题不是仓库不够多，而是需要更清楚地知道哪些仓库代表未来，哪些仓库只是通往未来路上的实验。
