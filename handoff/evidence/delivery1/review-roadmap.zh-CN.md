# D6 Task/Work 编码长度小批独立审阅

结论：**STATIC CLEAR，可在 Root 完成 fmt、strict 和直接相关 native 后提交。** 未发现本差异引入的 confirmed/likely 问题。范围仅 4 个文件、4 处生产替换和 1 个边界测试。

- 审阅对象：正式分支 HEAD `d53d270785cc88fd59d75b556718739e20d71221` 上完整 git diff（41+/4-），并与冻结候选 SHA `2bbd9ca15ee5b51443a5521bc6b31fe288a95e1118bc2d618b77878d16e4e649` 核对。正式四叶均等于冻结 new 经 rustfmt 后结果。
- `QualifiedTaskFact::validate`、`QualifiedWorkRecord::validate`、`StoredTask::qualified`、`StoredRecord::qualified` 只把 `to_vec(...).len()` 改为既有 `serialized_bytes` 加 checked `usize` 转换。scope/key/revision/digest 顺序、资格、clone、事务、原错误门禁和 `resident_bytes` 阈值保留。
- 本地锁定 serde_json 1.0.149 的 `to_vec` 本就调用同一 compact `to_writer`；原 D6 计数 writer 完整计字节且 checked-add。因此改变的是临时正文 Vec 分配，未改变成功编码长度。`usize` 无法表示结果时显式报错，不截断。
- 新 Work 测试用原 Vec 编码作独立基准，包含转义/control/NUL/Unicode 文本；exact、+1 通过，−1 拒绝，foreign scope 仍拒绝。未修改现有测试断言。
- 独立检查 PASS：冻结 patch/8 个 base-new SHA、D6 git baseline、四叶 stdin parse、正反逐字 replay、正式格式后逐字对应、git diff whitespace；既有 helper 文件逐字未变。未带新 helper、ledger、Source、schema、角色或 phase API。

编译/native 由 Root 执行，审阅者未运行 Cargo 或测试。真实退出线是本小批 warning-free fmt/strict 与新边界/直接 Task、Work qualification 用例通过；本报告不将此变更描述为 native 材料、联合峰值或整个 D7 完成。

机械记录：`/private/tmp/holon-d7-encoded-validation-review-roadmap/checks.json`。
