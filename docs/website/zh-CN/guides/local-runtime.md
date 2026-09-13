---
title: 本地运行时
summary: 在本地运行和检查 Holon 的一套保守流程。
order: 10
---

# 本地运行时

当你想检查 Holon，又不假定每个接口都已稳定时，用这套流程。

## 1. 构建与测试

```bash
cargo build
cargo test
```

针对具体工作时，优先跑最相关的 Rust 测试目标，提交改动前再跑更广的检查。

## 2. 检查命令面

```bash
cargo run -- --help
```

运行时仍在演进。以编译出的帮助输出为准，它定义了你这份 checkout 的确切本地 CLI 行为。

## 3. 让生命周期概念保持可见

测试行为时，记录你正在验证哪个运行时概念：

- 工作项的创建或更新
- 任务生命周期与输出获取
- 排队和唤醒/休眠行为
- 外部触发器的接入
- 面向用户的投递与内部轨迹输出的区别

这样实验才会贴合 Holon 的产品意图，而不是退化成模型提示词试验。

## 4. 通过仓库检查验证

默认的 Rust 检查是：

```bash
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --all-targets
cargo test
```

迭代时可以用更窄的检查，但最终验证不要用一次性脚本替代真实的项目检查。
