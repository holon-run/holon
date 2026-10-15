# D7 最小 Resource 前置交付

Base: clean D6 d53d270785cc88fd59d75b556718739e20d71221，所有 base 逐字来自正式只读 worktree。只写本 stage，不写 authority/union/正式树，不运行 Cargo/native。

实际行为：D6 `build_loaded_context_prompt_once` 在已有 qualified selected Input scope 下，从实际 Runtime ledger 创建一个 **resource-only** read material；同一个材料跨 skills/catalog、plan/notes、AGENTS/agent memory 三个原同步文件阶段累计。每次 TLS enter 只包住同步读取，skills async registry lock 先 await，drop TLS 后再 await 其它阶段。原同scope ledger completion guard 在首 buffer extend、目录项/derived catalog collection、结构化 manifest/frontmatter parse 前观察原 8-copy 成本。可选 reader 原吞错策略保留，但 quota rejection 被 material latched，阶段 finish 必须拒绝，不变 Absent/空 catalog。无 active phase 的旧 caller 保原结果。

实际 guard 从 file preparation 留到 `EffectivePrompt._resource_read_material`，Clone/reprojection 沿原派生 Clone 分享同一 Arc，最后 prompt 释放才 drop。首片保原峰值、不 shrink/retire，不新增ledger/category/nonce框架、不重注册同份材料。原 plan创建/文件权限/SQL资格/错误/输出schema保持。

依赖仅 D6 已有 `InputEvidenceBudgetLedger::{limits,reserve_owned_completion}`、`OwnedCompletionBudget::{observe_material,guard}` 与原 `BudgetViewGuard`/Input scope；不依赖任何 D7 borrowed/native material、Current/Source、Task/RunOnce/Summary改造。核心 buffer/directory/parser gates来自exact81 `diagnostics/resource_io.rs`，裁掉 registry/version/digest/retained/seal/capability-deny/observer/Host物理writer依赖；只加可复用同步enter以串起D6现有实际caller。

**退出线**：只交付现有命名 Context 资源读取子阶段的 raw/native allocation gate 与实际 prompt guard lifetime。64MiB/65536 是原 ledger hardbound clamp 下的资源子额度，不证明整 Context phase；已同时持有 State/Task/Work/Input/SQL临时材料的联合 prior 尚未完成，不能以此宣布联合cap/零文件读完成。没有启用 versioned registry/Ready/observer；known-write/physical completion/install fences仍后继，不在本批冒关闭。未加入未证明的常数 prior或同ID折扣，未扩任何限额。

新增直接相关 tests（Rust/native UNRUN，交Root唯一构建）：
1. `diagnostics::resource_io::tests::actual_file_allowance_precedes_utf8_and_keeps_original_error_without_a_phase`：128KiB malformed actualfile/64KiB allowance，先ResourceExhausted；无phase仍原InvalidData，ledger归零。
2. `real_guidance_and_catalog_share_exact_allowance_and_last_guard`：真实AGENTS+SKILL目录共享phase、实际测量byte/node minus1拒与exact正例、optional quota不吞、lastArc/ledger0。
3. `one_material_can_resume_sync_reads_without_leaking_tls_across_the_gap`：分段真实文件读同额度累计，段间TLS absent。
4. `runtime::context_capture::resource_read_tests::loaded_context_keeps_one_resource_guard_through_prompt_clones`：真实D6Runtime+localAGENTS/SKILL+preview_prompt，不借机器HOME；实际prompt clone保原guard到最后holder。

建议相关回归：现有 agents_md / agent_memory / agent_template / skills registry + loaded Context/preview/provider prompt、Context snapshot/reprojection 用例。Root定点compile/check/test，非作者全文peer通过后才进入正式树。本stage parse/正反exact/whitespace/hash验证详见validation.json，未声明native通过。
