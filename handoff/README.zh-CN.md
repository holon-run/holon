# 未交付 D7 交接材料

本分支是参考库存，不能整体合入正式分支。继续开发从 `origin/codex/agent-runtime-state-rfc` 开始。

- 第一提交 `9b509ae1a4e568dfe3cd402cb45fba164d98f388`：原 exact81 源码快照，基于 D6 d53d2707。libtest 构建通过，但相关37项34通过3失败，原Result预算另失败；不是严格全目标通过。
- 第二提交：较新的私有组合，只有部分静态review，整体未构建/运行。详见manifest源码hash。
- `resource-candidate/resource.patch`：D6基线的15叶候选，尚未正式应用，尚未Cargo/native。`UNFINISHED-review-fixes.patch` 是关机时作者正在修改的未冻结草稿，需完整review与测试，不能直接信任。
- `unfinished-prototypes.tar.gz`：OwnedRunOnce/canonical material/WorkInput/runner_failed等未完成base/new与状态文件。解压后只作参考，不整体应用。
- `other-candidates/`：fresh-only NoState及未安装Result RFC追加；均不代表完成。
- `evidence/`：原source SHA、真实构建/native结果和两批正式提交的review。绝对本机路径仅说明历史来源；远程不能用这些路径作为活进程或可访问artifact证据。
- approved-overall-design是用户已批准的原总体方案；历史实施状态以正式分支最新handoff为准。

正式交接入口：`docs/handoffs/agent-runtime-state-2026-10-15.zh-CN.md`。原goal完整D7/D8验收条件不缩减，不包含密钥、数据库或生产Agent数据。
