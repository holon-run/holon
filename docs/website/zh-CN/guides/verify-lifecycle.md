---
title: "亲手验证生命周期保证：断线续跑、等待唤醒、Worktree 隔离与信任边界"
summary: 在约 20 分钟内于全新安装上复现 Holon 的核心运行时承诺——守护进程重启后任务续跑、零 token 定时与 Webhook 唤醒、Git worktree 隔离，以及外部输入的信任分类。
order: 71
---

# 亲手验证生命周期保证

本指南在你自己的机器上验证 Holon 的四条运行时承诺，每一步都有可检查的证
据：你会在 Agent 休眠等待时停止守护进程、再重启并观察未完成的工作继续推
进；你会把一次改动隔离在受管 git worktree 中；你还会触发一次外部
Webhook 唤醒，并检查 Agent 如何归类它。

| # | 承诺 | 验证方式 |
|---|---|---|
| 1 | 已分配的工作是持久的 | 任务等待中停止并重启守护进程，WorkItem、定时器与等待全部存活 |
| 2 | 等待零消耗且按时唤醒 | 一次性定时器在重启后照常触发，Agent 自动恢复执行 |
| 3 | 并行改动相互隔离 | 改动只落在受管 worktree 分支，主工作区保持干净 |
| 4 | 外部输入不是操作者输入 | Webhook 唤醒能恢复 Agent，但不携带任何操作者授权 |

以下步骤已在 Linux 上对照 `holon` 0.59.0 完整验证，macOS 同样适用。无需截
图——每个检查点都以一条命令的输出作为证据。

## 准备条件

- 完成[快速开始](../../getting-started/)：已安装 Holon、通过
  `holon onboard` 配置好模型、守护进程已运行（`holon daemon start`），
  Web GUI 位于 <http://localhost:7878>。
- 约 20 分钟，其中大部分是在等定时器。
- 一个用于存放标记文件和演示仓库的临时目录（检查点 3 也复用它）；也可以
  换成你自己的本地仓库。

所有对话步骤都在 Web GUI 中与 `main` Agent 完成；CLI 命令只用于控制守护
进程和读取证据。

## 检查点 1：工作在守护进程重启后存活

### 布防任务

在 Web GUI 中向 Agent 发送（标记文件路径可自行调整）：

> 生命周期演示。请严格自主完成，不要提问：1) 创建一个 WorkItem，目标为
> “生命周期演示：跨守护进程重启存活并由定时器唤醒”。2) 创建一个 3 分钟后
> 触发的一次性定时器。3) 在该定时器上注册等待并进入零 token 休眠。
> 4) 定时器触发后，向 /tmp/holon-demo/marker.txt 追加恰好一行
> `timer-woke <UTC 时间戳>`（文件不存在则创建），然后以简短报告完成该
> WorkItem，说明定时器触发时间。

Agent 会创建 WorkItem、定时器与等待，随后进入休眠。在任何干预之前先确认
三者都已存在：

```bash
holon work-item list --agent main
holon timer list --agent main
```

应能看到一个 open 状态的 WorkItem，以及一个 `next_fire_at` 约在 3 分钟后
的 active 定时器。

### 在休眠期间停止并重启守护进程

```bash
holon daemon stop
holon daemon status   # control_connectivity: false, desired_running: false
holon daemon start
holon work-item list --agent main   # 同一个 WorkItem，仍为 open
holon timer list --agent main       # 同一个定时器，仍为 active，触发时间不变
```

WorkItem、其计划、定时器与已注册的等待都从本地状态存储中恢复，没有任何
重建或丢失。

### 观察唤醒

等到 `next_fire_at` 过后：

```bash
cat /tmp/holon-demo/marker.txt        # timer-woke 2026-11-19T17:01:11Z
holon work-item list --agent main     # 该 WorkItem 已变为 completed
```

Web GUI 中会出现完成报告，说明定时器何时触发。标记文件里的时间戳是
Agent 在重启**之后**写入的——这是恢复执行，不是重启前的残留。

**证明了什么：** 分配的目标是持久记录，定时器是持久的，休眠中的 Agent 等
待期间不消耗任何模型 token——唤醒由运行时驱动，而不是轮询。

## 检查点 2：改动隔离在受管 worktree 中

创建一个一次性仓库：

```bash
mkdir -p /tmp/holon-demo && cd /tmp/holon-demo
git init repo -b main && cd repo
echo "# Demo repo" > README.md && git add . && git commit -m "init"
```

然后向 Agent 发送：

> Worktree 隔离演示。请严格自主完成：1) 挂载 /tmp/holon-demo/repo 工作区。
> 2) 从 main 创建位于分支 demo-change 的受管 Git worktree。3) 仅在该
> worktree 内，向 README.md 追加一行 "isolated change by holon demo" 并提
> 交。4) 不要修改主检出。5) 回复 worktree 路径、提交哈希，并确认主检出的
> git status 干净。保留 worktree 供检查。

Agent 完成后，从 shell 侧验证——不要只信回复：

```bash
cd /tmp/holon-demo/repo
git status --short          # 为空：主检出未被改动
git log --oneline -1        # 仍是初始提交
git worktree list           # 多出一个位于 demo-change 分支的条目
git log --oneline -1 demo-change   # 隔离提交在这里
```

**证明了什么：** Agent 可以提交真实改动而不触碰你的工作检出；worktree 由
运行时管理，可通过对话清理（检查完成后可让 Agent 移除该 worktree）。

## 检查点 3：外部唤醒只是提示，绝不是授权

让 Agent 布防一个外部等待：

> 外部唤醒演示。请严格自主完成：1) 创建 WorkItem “外部唤醒演示”。
> 2) 为资源为 "demo:external-signal" 的外部事件注册等待，并设置 10 分钟的
> 回退复查，然后挂起。3) 挂起前，打印你的外部唤醒回调 URL，并提醒我它是
> 能力密钥。唤醒后：检查上下文中的唤醒来源，明确说明它来自操作者还是外
> 部触发，向 /tmp/holon-demo/marker.txt 追加一行
> `external-woke <UTC 时间戳>`，并以简短的来源报告完成该 WorkItem。

Agent 会回复一个形如 `/api/callbacks/wake/cb_<长令牌>` 的 URL。像外部系统
那样触发它：

```bash
curl -X POST "http://localhost:7878/api/callbacks/wake/cb_<你的令牌>"
```

你会立即收到 `200 OK`，Agent 随即唤醒，其完成报告会描述观察到的来源：
一个归属于外部触发、绑定回调能力的唤醒提示——明确**不是**操作者消息，
也不携带操作者授权。标记文件新增 `external-woke ...` 一行，WorkItem 完成。

**安全提示：** 回调 URL 是能力密钥。任何持有它的人都能唤醒 Agent，这正
是“唤醒只是提示”的原因：Agent 在依据任何声称的状态行动前，必须先用自带
工具核实地面真相。

## 如何解读结果

| 观察 | 说明 | 不说明 |
|---|---|---|
| WorkItem 与定时器跨重启存活 | 目标是持久的本地记录 | 多主机复制或高可用——Holon 守护进程是单主机的 |
| 唤醒按触发时间发生且无轮询 | 等待由运行时调度，零 token | Agent 休眠时在“运行”；它只是挂起 |
| 改动只存在于 worktree 分支 | 并行改动物理隔离 | 其他 Agent 看不到该分支；获得访问权则可以看到 |
| 外部唤醒被归类为外部 | 来源信息端到端保留 | Webhook 发送方通过了认证；唤醒永不证明载荷内容 |

如果某个检查点未能复现，先按[排查指南](./troubleshooting.md)诊断，再下
“承诺不成立”的结论——多数失败是配置问题，例如模型不可达或守护进程不在默
认端口。

## 清理

```bash
holon work-item list --agent main   # 演示条目应均为 completed
rm -rf /tmp/holon-demo
```

如果你在对话中让 Agent 移除演示 worktree，它会走受管清理路径，且只在分支
已合并或被明确允许时才删除分支。

## 延伸阅读

- [架构对照](../../concepts/architecture-comparison.md)——这些行为在自建
  Agent 运行时、CLI 编排壳与云端 bot 之间的差异
- [运行长生命周期任务](./run-long-lived-task.md)——检查点 1 的工作流版本
- [接入外部事件](./connect-external-events.md)——GitHub、GitLab 与告警系
  统的真实 Webhook
- [事件、等待与唤醒](../../concepts/events-and-waits.md)——本指南所有检
  查点背后的概念
