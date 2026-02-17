# Runtime Tools Contract 重构方案

## 1. 背景与问题

当前 `buildComposedImageFromBundle` 在 `pkg/runtime/docker/runtime.go` 内部内联了工具安装逻辑（Node、git、gh、gh-webhook 等），存在几个问题：

1. 工具契约是隐式的，没有正式定义哪些工具是必须有的。
2. 安装逻辑硬编码在字符串 Dockerfile 中，维护成本高，扩展困难。
3. 当前 required tools 列表未正式化，难以作为稳定契约演进。
4. 缺少正式契约导致工具保证边界不清晰，出现运行时不确定性。

## 2. 目标

1. 定义正式的 Runtime Tools Contract（仅 required）。
2. 将 composed image 构建流程改为“内置契约驱动”，由 Holon 内部维护 required tools 列表。
3. 保持默认行为可用：始终使用内置默认 required tools。
4. 通过 fail-fast 明确缺失工具，降低运行时不确定性。

## 3. 非目标

1. 不在本阶段实现“容器启动后在线安装工具”。
2. 不在本阶段支持所有包管理器生态（先覆盖 apt/dnf/yum）。
3. 不在本阶段做复杂版本求解（先支持固定版本/latest）。
4. 不为契约提供项目级配置或 CLI 覆盖能力。

## 4. 契约定义（v1）

新增文档：`docs/runtime-tools-contract.md`（后续实现时补齐）

契约只维护一个 `required` 列表（v1）：

- `bash`, `git`, `curl`, `jq`, `ripgrep`, `findutils`, `sed`, `awk`, `xargs`
- `tar`, `gzip`, `unzip`
- `python3`
- `node`, `npm`
- `gh`
- `yq`
- `fd`（Ubuntu 实际包为 `fdfind`，需要别名）
- `make`, `patch`

## 5. 配置模型

不提供用户配置入口。工具契约由 Holon 内部固定维护（代码内置常量 + 文档）。

## 6. 代码重构设计

## 6.1 新增模块

新增 `pkg/runtime/tools`（建议）：

1. `contract.go`
- 定义 `ToolContract`, `ToolSpec`。
- 提供内置 `required` 列表（单一事实来源）。

2. `resolver.go`
- 根据基础镜像可用包管理器生成安装计划 `InstallPlan`。

3. `installer_script.go`
- 生成安装脚本片段（apt/dnf/yum）。
- 处理 `fd`/`fdfind` 兼容和软链。

## 6.2 `buildComposedImageFromBundle` 重构点

将 `runtime.go` 中当前内联 Dockerfile 拼接拆为：

1. 读取内置 tools contract（单一来源）。
2. 生成安装计划（仅 required）。
3. 生成结构化 Dockerfile 片段：
- base setup
- install required tools（失败即退出）
- 解包 agent bundle
4. 保持现有镜像 tag 计算方式和缓存语义不变。

## 7. 失败策略

1. required tools 安装失败：构建失败，报明确错误。
2. 无支持包管理器：如果 required 中有未满足项，则失败并输出缺失列表。

## 8. 测试计划

### 8.1 单元测试

1. contract 常量与解析测试（内置 required 列表）
2. install plan 测试（apt/dnf/yum/unknown）
3. installer script 生成测试（required 安装路径）
4. required 缺失时报错信息测试

### 8.2 集成测试

1. 默认内置契约下 composed image 构建成功
2. required tools 缺失时自动安装（支持包管理器）
3. 故意注入不存在工具时 required fail-fast（测试桩）

## 9. 迁移与兼容策略

本次按“无兼容包袱”执行（项目尚未正式发布）：

1. 保留旧逻辑仅作为过渡分支内实现细节，不保留外部行为承诺。
2. 文档统一切换到 Runtime Tools Contract 概念。
3. 相关 prompt/skills 文档改为依赖内置 required 契约，不再引用能力文件。

## 10. 实施分解

1. 第一步：落地 contract 数据结构和内置 required 列表
2. 第二步：重构 composed image 构建为内置契约驱动安装
3. 第三步：文档与 prompt 更新，补齐 e2e 用例

## 11. 验收标准

1. `run/solve/serve` 在默认契约下行为稳定且可重复。
2. 自定义镜像场景下，Holon 根据内置 required 列表自动补齐（在支持的包管理器上）。
3. required 不满足时 fail-fast，错误信息可诊断。
4. CI 覆盖 required 的成功与失败路径。
