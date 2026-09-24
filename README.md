已写入 `/workspace/github_bounty_hunter/README.md`。在 `## Install` 和 `## Provider setup` 之间新增了 `## Connect from mobile` 章节，覆盖：

- Tailscale Serve 连接拓扑图
- 前置条件（MagicDNS、HTTPS certs、ACL）
- `holon serve --listen 127.0.0.1 --advertise https://<device>.<tailnet>.ts.net` 配置
- `tailscale serve https / http://127.0.0.1:7878` 命令
- WebSocket/SSE/认证/文件访问验证清单
- 7 项故障场景与恢复路径表格
- 无 Tailscale 的局域网替代方案与限制
- `holon serve --help` 提示