# AgentInbox CLI Quickstart For pre-public runtime (2026-04)

这份文档不是架构说明，而是给 `pre-public runtime` agent 的最小操作手册。

目标只有一个：

- 让 agent 知道如何用 `agentinbox` CLI 完成
  - 创建 fixture source
  - 创建 subscription
  - 等待 wake
  - 读取 inbox item

## 1. CLI 路径

当前联调使用这个 CLI：

```bash
node /Users/jolestar/opensource/src/github.com/holon-run/agentinbox/dist/src/cli.js
```

联调时，每条命令都要带：

```bash
--home /tmp/agentinbox-demo
```

## 2. 创建 fixture source

命令：

```bash
node /Users/jolestar/opensource/src/github.com/holon-run/agentinbox/dist/src/cli.js \
  source add fixture demo \
  --home /tmp/agentinbox-demo
```

返回里最重要的是：

- `sourceId`

后续 `subscription add` 和 `fixture emit` 都会用到它。

## 3. 创建 subscription

命令模板：

```bash
node /Users/jolestar/opensource/src/github.com/holon-run/agentinbox/dist/src/cli.js \
  subscription add <agent_id> <source_id> \
  --home /tmp/agentinbox-demo \
  --match-json '{"channel":"engineering"}' \
  --activation-target '<runtime-incubation_callback_url>'
```

返回里最重要的是：

- `subscriptionId`
- `inboxId`

默认情况下，`inboxId` 一般会是：

```text
inbox_<agent_id>
```

## 4. fixture event 注入

这一步通常由测试驱动来做，不一定由 agent 自己执行。

命令模板：

```bash
node /Users/jolestar/opensource/src/github.com/holon-run/agentinbox/dist/src/cli.js \
  fixture emit <source_id> \
  --home /tmp/agentinbox-demo \
  --metadata-json '{"channel":"engineering","kind":"review"}' \
  --payload-json '{"text":"PR #123 received a new review: LGTM with one nit"}'
```

如果需要显式 materialize：

```bash
node /Users/jolestar/opensource/src/github.com/holon-run/agentinbox/dist/src/cli.js \
  subscription poll <subscription_id> \
  --home /tmp/agentinbox-demo
```

## 5. 读取 inbox

先列 inbox：

```bash
node /Users/jolestar/opensource/src/github.com/holon-run/agentinbox/dist/src/cli.js \
  inbox list \
  --home /tmp/agentinbox-demo
```

再读取具体 inbox：

```bash
node /Users/jolestar/opensource/src/github.com/holon-run/agentinbox/dist/src/cli.js \
  inbox read <inbox_id> \
  --home /tmp/agentinbox-demo
```

agent 在 wake 之后，应该优先执行这两步，而不是回头去读仓库代码或写测试。

## 6. 当前联调的正确顺序

当前这轮联调，agent 应该按下面顺序做：

1. 创建 `pre-public runtime` callback，要求 `delivery_mode = wake_only`
2. 执行 `source add fixture demo`
3. 执行 `subscription add`
4. 记录：
   - `sourceId`
   - `subscriptionId`
   - `inboxId`
5. 停下来等待 wake
6. 被唤醒后执行：
   - `inbox list`
   - `inbox read <inbox_id>`
7. 总结最新 inbox item

## 7. 当前联调里不该做的事

这轮联调里，不该做这些事情：

- 不要自己发明新的 `AgentInbox` API
- 不要去写 `pre-public runtime` 测试文件
- 不要去修改 `fixture` JSON
- 不要在 wake 后重新探索仓库
- 不要把 callback/wake 流程改写成测试开发任务

这轮的任务是运行系统，不是改系统。
