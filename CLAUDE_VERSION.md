# Claude Code 版本管理

## 当前固定版本

- **@anthropic-ai/claude-code**: `2.0.72` (固定于 2025-12-18)

## 固定原因

为了避免 CI 环境中的行为漂移，提高构建稳定性，项目中所有对 `@anthropic-ai/claude-code` 的安装都使用固定版本。

## 固定位置

1. **images/adapter-claude/Dockerfile** (第19行):
   ```dockerfile
   RUN npm install -g @anthropic-ai/claude-code@2.0.72
   ```

2. **pkg/runtime/docker/runtime.go** (第189行):
   ```dockerfile
   RUN npm install -g @anthropic-ai/claude-code@2.0.72 && \
   ```

## 版本更新流程

当需要升级 Claude Code 版本时，请：

1. 检查新版本的兼容性
2. 更新上述两个文件中的版本号
3. 测试构建：`make build-adapter-image`
4. 更新本文档中的版本信息
5. 提交变更时说明版本更新原因

## 相关文件

- `images/adapter-claude/.claude.json` - Claude Code 配置文件
- `Makefile` - 构建脚本，包含 `build-adapter-image` 目标