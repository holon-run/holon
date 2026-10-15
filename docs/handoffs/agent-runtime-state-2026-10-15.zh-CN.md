# Agent 内存状态改造交接（2026-10-15）

本机因用户赶飞机需要关闭，停止本机实施并交给远程 agent。**整个 goal 未完成。** 用户最新要求：不采用 `code-health-audit`；收敛已有实现，按可独立验证的小批次实施、review、提交，最后完整验收后创建一个整体 **draft PR**。不自动转 ready、合并或部署。

## 接手入口

- 正式分支：`codex/agent-runtime-state-rfc`。先从此分支继续；本文所在提交包含全部正式进度。
- D7 参考源码分支：`codex/agent-runtime-state-d7-wip-handoff-20261015`。它保存本机未交付实现，**不是已验收实现，不能整体合并或把静态 review 当作运行通过**。
- WIP 分支第一提交是原 exact81 快照，父提交为 D6 `d53d270785cc88fd59d75b556718739e20d71221`；第二提交保存更新的私有组合和 `handoff/` 下的候选、草稿与证据。
- 正式方案：[Agent Runtime State RFC](../rfcs/agent-runtime-state-and-context-snapshots.md)。仓库内 RFC 只描述方案；实施状态与任务写在本文。
- 原关联 issue：[#3478](https://github.com/holon-run/holon/issues/3478)。模型提出 semantic summary/retirement/supersedes/cross-agent memory 仍不在本轮范围；不能因内存状态基础落地就宣称整个 issue 的所有扩展都完成。

远程接手建议先执行：

```sh
git fetch origin codex/agent-runtime-state-rfc codex/agent-runtime-state-d7-wip-handoff-20261015
git log --oneline origin/codex/agent-runtime-state-rfc
# 在独立 worktree 继续正式分支；从 WIP 分支按实际调用链抽取所需改动。
# 不要把 WIP 整体 merge/cherry-pick 到正式分支。
```

WIP 分支的 `handoff/README.zh-CN.md` 和 `handoff/manifest.json` 给出精确 SHA、来源、验证状态和候选文件。本文及 WIP 内容均不包含 API key；远程节点须使用其获准、已配置的凭据，真实 LLM 验收不能用旧基线代替。

## 已正式交付

D0–D6 已提交并分别 review。关键阶段提交：

| 阶段/批次 | 提交 | 内容与实际证据 |
| --- | --- | --- |
| D5 | `e2d076e6` | Task/result 资格与 resident 发布；阶段记录保存在 WIP handoff 证据。旧 D5 全量一次出现 socket WouldBlock，孤立重跑通过；不能把该次全量称为全部通过 |
| D6 | `d53d2707` | qualified Scheduling resident 发布；定向 242 项、补充 26 项及最后 1 项通过，严格全目标编译通过 |
| D7 独立批 1 | `67bf0a956a665acb313a6bece12dc4913d5655cd` | Task/Work qualify/validate 四处复用既有流式字节计数，移除临时整份 JSON Vec；4 文件 +41/-4。fmt 通过 5.29s；`RUSTFLAGS="-D warnings" cargo check --all-targets` 通过 90s；libtest 构建通过 154.17s；9/9 直接相关测试通过 1.62s；Roadmap 独立 review 通过 |
| D7 独立批 2 | `e44c17fe46a27b8f9d2b33226f4499a602af1af8` | bootstrap 无原 child-exit 证明时保留 worktree；原 parent-less obligation 不因 Result 合成 Turn 被升级为 rejoin fence；删除唯一无调用的私有 Runtime Git cleanup 链，Frozen/Completion policy 保持。4 文件 +296/-219。fmt 通过 5.15s；严格全目标编译通过 29.66s；libtest 构建通过 76.09s；7/7 相关测试通过 2.05s；Episode 独立 review 通过 |

两批 native 都使用默认栈，源码和二进制前后核对未变化。最终正式源码到 `e44c17fe` 之后仅增加交接文档；关机收尾未执行新的产品改动、真实 Agent 删除、重建或 LLM 调用。

## 原 D7 库存的真实状态

原私有 exact81 相对 D6 有 468 个改动文件、245 新文件、+107598/-4746 行。用户已要求收敛，**规模本身不证明这些代码可删除，也不是一次交付**。当前从已提交分支逐片抽取实际完整调用链，相关验证和独立 review 后立即提交，避免继续累积巨型未提交组合。

exact81 composite SHA：`d5296504200a1e28c0780c48f3c8acf1f9475e5b320932baa619db9c39026294`。它仅通过 libtest 构建（159.19s），不是严格全目标验证。最新核心 37 项为 34 PASS/3 FAIL（49.89s）；另一个原 Result 预算回归单独 FAIL。原完整 132 条回复用例之前已通过，保持原 132/128 窗口和限额。

未通过项及私有修正：

1. Cold 两个用例把真实 Work Host-address guard 当作 scoped guard，报 ForeignMaterial。私有 `83e90a…` + drop-order `daea74…` 明确来源，禁止 fallback/relabel；静态审阅通过，**native 未跑**。
2. Command 顺序测试查询完整 Audit envelope 的错误 JSON 层级；`bb2e62…` 将 `$.task_id` 改为 `$.data.task_id`，仅 fixture，**native 未跑**。
3. 原 96KiB Result、4MiB 正例失败：written sizing 前 prior 为 3970281 bytes/4670 nodes。原 4MiB 正例、3MiB 负例、8MiB 重试及 96KiB payload 不得缩改。`41bbaed…` 的退役修正单独不安全，必须与 `f83ac525…` retained-command companion 和 `2133998…` cfg 修正联合；已全文联合静态 review，**native 未跑**。
4. generic NoMemory 最终 Input publication：`a7a8db…` + `c32ec67…` 仅支持已核的原 generic path；specialized writers/M2/Work 仍未闭合，**native 未跑**。
5. 私有组合的 RFC 末尾两个 Result 追加尚未安装；`375b79…` 保留为候选。不要把“冲突列表为空”等同于所有必要修改都已加入。

WIP 最新组合尚未整体构建/运行。旧 prepared82/145 runner 没有执行，不要把它们当验证结果，也不要因旧脚本存在就重启整套巨型流水线。

## 下一批：资源读取准备（尚未应用正式分支）

原候选 `resource.patch` SHA256：`3aad6f3f882d358c91296a005b680e29aa071fa3a898b774ec19ad9722d0b2d6`，15 文件 +535/-22，包含四个直接相关测试。真实 D6 Context 的 skills/catalog、plan/notes、AGENTS/curated memory 三个同步文件阶段共用一个资源读取材料；TLS entry 不跨 await，最后 Prompt/Clone/reprojection holder 保留 guard。

它只交付 **资源读取子阶段**，没有建立版本 registry/observer/physical install fence；整 Context 的 State/Work/Task/Input/SQL 同时存活 prior 仍未闭合。不能以此宣称全 Context 联合限额或零文件读。

Root 和独立 reviewer 对原片已全文阅读，以下收口尚未完成最终冻结/编译/review：

- 首 header 的标量预算检查须在 `reserve_owned_completion` 的 ScopeKey clone、guard/Arc/Mutex 分配之前。
- `io::ErrorKind::Other` + `ResourceExhausted` 字符串会映射成 `io_error`、不可重试。使用既有 typed RuntimeError 分类，真实 `describe_runtime_error` 测试证明稳定 code/可重试；不用新增通用错误框架。
- 可选 `.ok()/flatten()` 不能将 quota 变为 Absent。所有同步阶段先检查 failed latch，再传播原非 quota reader 错误；原 `?` 提前退出路径也须覆盖。
- Host warm SkillsRegistry 的 `catalog_from_entries` 首 `name.clone`、完整 entry clone 和 `skills_runtime_view_from_catalog` 的 catalog clone 未走文件 hooks；若保留 catalog collection gate 承诺，必须在真实首 clone 前计量，不能只测冷文件路径。
- RFC 新段改为英文纯方案，不含“D7 独立前置”、阶段状态或实施顺序。状态留本文。

作者正在写的单个 companion 已随 WIP handoff 保存为 **未完成草稿**。不要当作已审/可直接安装补丁。接手先完成此片的最终完整 review，再应用正式分支，fmt、严格全目标编译和直接相关测试通过后独立提交。

## 后续仍须完成的 D7 调用链

按实际依赖抽取，必要范围可接受，但不要再整片搬入 468 文件：

1. **版本化 core resource → 实际 loaded Context 纯读取。** AGENTS/curated memory/skills/catalog/notes 的 source/workspace/config/incarnation、immutable body、digest、epoch/generation；合法 Absent、Dirty、RefreshFailed 区分；mandatory Dirty 拒新 capture；旧 snapshot 稳定；旧 refresh 不覆盖新 generation。原 known-write、Host、真实物理 writer-exit 与 install fence 必须接入；caller cancel 不能提前释放责任。Work plan/report 保持明确 adapter，单独后续收口。core 新抽取任务在关机前未产生候选，不计交付。
2. **统一 acquisition 与真实预算生命周期。** canonical identity/State 在首次解码/clone 前计量；外层请求、related identity、facts、Unknown 恢复和 Root State 的实际 guard 保活。Host/scoped guard 来源明确，禁止改标或相同 ID/body 折扣。复用现有 Material 原语，按真实生命周期退役临时 scratch，保持完整 command/ACK。
3. **Current/普通 reader 全 writer 闭合。** State/Queue/Event 资格与同 publication cut；原 Audit、生命周期与 observation writers 必须维护或显式 invalidation；receipt/OCC/Unknown/replacement/Closing 保留。未闭合路径不能伪造 Ready。
4. **Input 查询发布。** generic/Evidence/Control/Stop 四类原 TX、M2/Work 最早及最终阶段共享实际 Input Native phase；原 Source cursor/live cut、roles、required roots 与 owning closure 保持。已有 bootstrap SourceSome 不代表全部 body writer 增量维护完成。
5. **RunOnce 真正消费者。** 原产品路径仍 `all_messages/all_tools/all_events` 扫历史。使用同 cut finite born-membership、完整所有 born Task heads 与旧 reply 并集；scoped Transcript Present/Missing 与既有 assistant selection 算法保持。单 Task recall 不能循环代替完整 born union；没有 64/128 LIMIT 捷径。响应 material/Bytes holder 到最后 wire slice。
6. **Turn 与 GenerateImage。** owning membership、逐 Turn Message/Brief/Tool 的旧 I/O 及 recent64 大 Tool body 附件 adapter 仍待切换，保持原排序/权限/精确 locator。
7. **Loaded/Cold API 完整闭合。** full Summary 的 execution/occupancy/notifications、mixed cold chain、State HTTP Bytes 保活、Current Event 与历史 backscroll/SSE、fleet 逐入口交付。已有 skills/Scheduler/compact/all-loaded child 的局部 PASS 不代表完整 endpoint 纯内存。
8. **冷子 Agent。** actual qualified finite loaders、原 Header/Work/Scheduling/Input/Task/Current、DTO→wire 与真实 domain body loan。NoState 候选 `4404fc…` 仅 fresh-empty，legacy complete-empty matrix 未做；不激活 cold Runtime、无 State 不读取其他域巨体、foreign/pruned 严格拒绝。
9. **原 runtime observation Audit 与物理续责。** Command/Work warning/Skill state audit 已有局部覆盖；runner_failed、完整 writer inventory 仍待收口。保持原 State ACK 与独立 Audit 次序、原 payload/receipt 和 continuation ownership。
10. **上游整合与文档。** 本机调查时 local main 为 v0.52.0、migration max82；feature migrations82–93 有编号冲突。重新 fetch 后核实际 upstream，保 upstream82 与真实 v81 cutover，不盲目重命名。RFC 已过长，最终去除 chronology/重复追加，仅保方案；实施记录留 handoff。

WIP `handoff/` 同时保存 OwnedRunOnce、canonical material、Work/Input 和 runner_failed 等未完成原型及明确 WIP 状态。它们没有统一接线、编译或 review，不因文件存在而计完成。

## 已确认的范围与验收合同

- Agent 核心状态在内存；数据库负责同一事实的持久化与恢复。candidate 在内存先计算，业务事务 ACK 后才发布成功并允许 effect；提交结果未知先恢复原结果，不重做业务。
- transient resources 不序列化句柄；外部资源承诺 latest observed version，非外部写入瞬间可见。context 是 agent state 的一致投影。
- 首次升级承接 existing live work/obligations 与所需 continuity；同名身份重建隔离旧 continuity。raw/foreign/pruned 不补造权限；Task Cancelled、monitor join、身份退役不等于子进程退出或获准删除 worktree。
- accepted pending 正文不得丢弃；超预算 activation 明确 ResourceExhausted，保 durable 数据，调整限额后重试。不静默放宽或逐条冷读伪装 Ready。
- 普通 WorkItemScoped64（配置上限512）与 agent fallback 并集、recent默认12、原 Task terminal128、原 Result/132 fixture 均保留。历史增长不能触发普通请求的新 hydration。
- 当前事实、accepted payload、history、pins、observations、indexes、candidate 分别核算；真实同 allocation 在 Host 账去重，selected 不保整个 Root；RSS 单列。

D8 **未开始**，不能用阶段 PASS 替代：

1. 最终精确提交跑完整回归、全 writer/command outcome 与跨 owner fault matrix：rollback、真实 commit 后 lost ACK、后续状态推进重放、publication 失败、caller cancel、Host close、replacement、重启/重建、权限撤销、prune/hydration、legacy 超限。
2. acquisition/selection/capture/render/request construction 全链普通 SQL/文件读 capability-deny 与实际归因交叉证明。Bootstrap、validated recovery、显式 recall、resource refresh、事务内校验分别列原因，不能排除整个 ingress/准备后宣称零读。
3. 固定 D0 release 基线 `3637f8b0`，正常8 loaded/4并发，压力32/16；100 pending ×16KiB，100k history/64 linked sets，10MiB stdout+小preview、慢请求与 pins。每类每轮≥100样本、3轮。context→request **和 commit/publication** p95≤1.10 baseline，吞吐≥0.95；启动/恢复峰值单列，不倒推放宽阈值。
4. 离线 e2e 后做 **新的真实 LLM** 验收：既定 DeepSeek/deepseek-flash、`api.deepseek.com/anthropic` 同 route、隔离 Host；每组最多12实际外发（含retry/fallback/group），每次输出≤512 tokens、wire body≤128KiB。旧基线5 cases/10 wire200、input34067/output318/12.265s只作对照。报告真实次数、行为、usage/cache，不凭小样本宣称成本下降。不发布密钥。
5. 最终 `cargo fmt --all -- --check`、`RUSTFLAGS="-D warnings" cargo check --all-targets`；移除旧路径，完成迁移文档及独立 review；全部门槛通过后 push 并开一个整体 **draft PR**。

平时只跑新增、失败和直接受影响核心测试；fmt/strict/独立 review 后逐批提交。最后统一完整验收，不每片重复全量、100001 历史或 10MiB 大输出。工程复杂度不是阻塞项；只有真实产品/权限/保留语义改变、冻结负载下无法满足合同、未授权外部操作或用户改动冲突才需讨论。
