---
title: 用自托管 Casdoor 配置 OIDC 单点登录
summary: 自托管 Casdoor 作为 Holon 的 OIDC 身份提供商：Docker 部署、初始化组织与应用、配置 Holon 并完成端到端验证，全程不依赖任何外部账号。
order: 188
---

# 用自托管 Casdoor 配置 OIDC 单点登录

Casdoor 是 Casbin 团队开源的集中式身份与访问管理平台（Apache-2.0），自带管理 UI，支持 OAuth 2.0、OIDC、SAML、LDAP、SCIM 等协议。作为 Holon 的 OIDC 身份提供商时，组织、用户、应用、会话策略全部在你自己的 Casdoor 里管理，不需要向任何第三方注册。相比 Keycloak，Casdoor 部署更轻（单容器 + 一个数据库），且从 v4.26.0 起支持「组织作为独立 issuer」的路径形 discovery。本文给出从部署到端到端验证的完整流程，全部步骤在一台机器上即可复现。

通用概念（Issuer、Client ID、回调地址、会话 TTL、审计归属）见[配置 OIDC 认证](./configure-oidc-authentication.md)。本文只补充 Casdoor 特有的部分。

## 前置条件

- 一台能跑 Docker 的机器；Casdoor 官方推荐 MySQL 8 作为持久化后端。
- 一个指向本机的 HTTPS 域名（本文示例用 `https://casdoor.example.com:9445`，可用 tailscale 内网域名 + tailscale cert，或公网域名 + Let's Encrypt，任意一种均可）。
- Holon 任意近期版本（需支持 `auth.oidc.*` 配置）。
- Casdoor **v4.26.0 或更高**（2026-11 实测版本；组织级路径 issuer 需要 v4.26.0+）。

## 第 1 步：部署 Casdoor

关键概念：Casdoor 容器内部监听 `8000`（HTTP）。环境变量 `origin` 必须等于浏览器最终访问的外部地址，否则 discovery 里的 `issuer` 和授权跳转地址都会指向错误位置。TLS 终止推荐放在前置反代（本文用 nginx sidecar + tailscale 证书），Casdoor 本体保持 HTTP。

三个容器按顺序启动：MySQL → Casdoor → nginx（nginx 必须最后启动，否则解析不到 upstream 主机名）：

```bash
docker network create casdoor-net
docker run -d --name casdoor-mysql --network casdoor-net \
  -e MYSQL_ROOT_PASSWORD="<数据库 root 密码>" \
  -v "$PWD/mysql-data:/var/lib/mysql" \
  mysql:8.0

# 等待 MySQL 就绪：必须用 TCP 探测（-h 127.0.0.1）。
# 首次初始化期间 MySQL entrypoint 起的是 socket-only 临时实例，
# 不加 -h 127.0.0.1 会提前返回成功，Casdoor 随后连库失败。
for i in $(seq 1 60); do
  docker exec casdoor-mysql mysqladmin ping -h 127.0.0.1 -uroot -p"<数据库 root 密码>" --silent && break
  sleep 2
done

# 持久化数据目录初始化过一次后，MYSQL_DATABASE 不会再被执行，
# 因此显式幂等建库：
docker exec casdoor-mysql mysql -uroot -p"<数据库 root 密码>" \
  -e "CREATE DATABASE IF NOT EXISTS casdoor CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"

docker run -d --name casdoor --network casdoor-net \
  -e driverName=mysql \
  -e "dataSourceName=root:<数据库 root 密码>@tcp(casdoor-mysql:3306)/" \
  -e origin="https://casdoor.example.com:9445" \
  -v "$PWD/init_data.json:/init_data.json:ro" \
  casbin/casdoor:v4.26.0
```

nginx TLS sidecar（证书路径按你的实际域名证书替换）：

```nginx
server {
    listen 9445 ssl;
    server_name casdoor.example.com;
    ssl_certificate     /etc/nginx/certs/tls.crt;
    ssl_certificate_key /etc/nginx/certs/tls.key;
    location / {
        proxy_pass http://casdoor:8000;
        proxy_set_header Host $host:9445;
        proxy_set_header X-Forwarded-Proto https;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    }
}
```

```bash
docker run -d --name casdoor-nginx --network casdoor-net \
  -p 9445:9445 \
  -v "$PWD/tls.crt:/etc/nginx/certs/tls.crt:ro" \
  -v "$PWD/tls.key:/etc/nginx/certs/tls.key:ro" \
  -v "$PWD/nginx-casdoor.conf:/etc/nginx/conf.d/default.conf:ro" \
  nginx:alpine
```

验证部署：

```bash
curl -s https://casdoor.example.com:9445/.well-known/openid-configuration | jq -r .issuer
# https://casdoor.example.com:9445
```

管理控制台在 `https://casdoor.example.com:9445/`，默认账号 `admin`（首次登录后立即改密码）。

## 第 2 步：初始化组织、应用与用户

两种方式任选：

**方式 A：管理控制台手动创建。**依次创建组织、组织内用户、应用；应用里设置回调地址（Redirect URLs）、授权类型（Authorization code + Refresh token）、令牌格式（JWT），记下应用的 Client ID 与 Client Secret。

**方式 B：`init_data.json` 自动初始化（推荐给可复现环境）。**Casdoor 启动时读取挂载到 `/init_data.json` 的声明式配置，一步建齐组织、用户、应用。最小可用示例（Client Secret 与用户密码请替换为你自己的值）：

```json
{
  "organizations": [
    {
      "owner": "admin",
      "name": "holon-org",
      "displayName": "Holon Org",
      "passwordType": "bcrypt",
      "defaultTokenFormat": "JWT",
      "useOrganizationIssuer": false
    }
  ],
  "users": [
    {
      "owner": "holon-org",
      "name": "alice",
      "type": "normal-user",
      "password": "<用户密码>",
      "displayName": "Alice",
      "email": "alice@example.test",
      "emailVerified": true
    }
  ],
  "applications": [
    {
      "owner": "admin",
      "name": "holon-app",
      "displayName": "Holon RP",
      "organization": "holon-org",
      "clientId": "holon-rp-<你的后缀>",
      "clientSecret": "<Client Secret>",
      "signinMethods": [{ "name": "Password", "displayName": "Password", "rule": "All" }],
      "grantTypes": ["authorization_code", "refresh_token"],
      "redirectUris": ["http://localhost:7880/api/auth/oidc/callback"],
      "tokenFormat": "JWT",
      "expireInHours": 168
    }
  ]
}
```

**组织级 issuer（v4.26.0+ 可选）：**把组织的 `useOrganizationIssuer` 设为 `true` 后，该组织应用的 discovery/issuer 变为路径形 `https://casdoor.example.com:9445/orgs/<组织名>`。Holon 对这种路径形 issuer 直接支持，无需特殊配置（见第 3 步）。

## 第 3 步：配置 Holon

全局 issuer（上面第 1 步的默认形态）：

```bash
holon config set auth.oidc.issuer_url https://casdoor.example.com:9445
holon config set auth.oidc.client_id holon-rp-<你的后缀>
holon config set auth.oidc.client_secret_env CASDOOR_OIDC_CLIENT_SECRET
holon config set auth.oidc.redirect_uri http://localhost:7880/api/auth/oidc/callback
holon config set auth.mode oidc
```

组织级路径 issuer（可选）：只有 `issuer_url` 不同，指向组织的 discovery 根即可：

```bash
holon config set auth.oidc.issuer_url https://casdoor.example.com:9445/orgs/holon-org
```

三个容易踩的坑：

- `issuer_url` 必须与 discovery 返回的 `issuer` **逐字符一致**（含端口；组织级还要含 `/orgs/<组织名>` 前缀）。
- `redirect_uri` 必须逐字符出现在应用的 Redirect URLs 白名单里：Casdoor 端填的是 `http://localhost:7880/api/auth/oidc/callback`，与 Holon 侧配置完全相同（允许 localhost 携带非 80 端口）。
- `client_secret_env` 是**环境变量名**，密钥值在启动 Holon 时注入，不写入配置文件：

```bash
CASDOOR_OIDC_CLIENT_SECRET="<Client Secret>" holon serve --port 7880
```

配置写入顺序与 Keycloak 相同：先设 `auth.oidc.*` 子键，最后设 `auth.mode oidc`（当前版本 `holon config set` 每次写入都会整份校验认证配置，顺序颠倒会被拒绝）。

## 第 4 步：验证

1. 打开 Holon 登录页，应出现「Continue with organization login」；点击后跳转 Casdoor 登录页（地址是你的 Casdoor 域名）。
2. 用组织内用户登录，回到 Holon 后检查会话：

```bash
curl -s http://localhost:7880/api/auth/session/me
# {"ok":true,"user_id":"oidc-…","display_name":"…","auth_method":"oidc"}
```

3. 登出后同一路径应返回 401。

**Casdoor 特有的 secret 语义（排错时先看这里）：**Casdoor token 端点对 Client Secret 的规则是「提供即必须正确」——Holon 按 confidential client 用 form 提交 `client_id + client_secret`，secret 错误会得到 `401 invalid_client`；如果把应用的 Client Secret 留空，Casdoor 在 PKCE 下会按 public client 放行。正常配置（应用设置 secret + Holon 配置 `client_secret_env`）不受影响，但混用「应用留空 + Holon 配错」会产生难排查的歧义，建议始终显式设置 secret。

## 与 GitLab CE / Keycloak / Google Workspace 的差异

| 维度 | 自托管 Casdoor | 自托管 Keycloak | 自托管 GitLab CE | Google Workspace |
| --- | --- | --- | --- | --- |
| 外部依赖 | MySQL 8（推荐持久化） | 无（内置 H2 适合评估） | 无（实例自带 DB） | 需要 Google Cloud 项目与组织 |
| IdP 定位 | 集中式 IAM，多协议（OIDC/SAML/LDAP/SCIM） | 专职身份平台，realm 级隔离 | DevOps 平台内置 OAuth | 云办公套件身份 |
| 组织准入 | 应用绑定组织，组织内管理用户 | realm 内用户管理；可接联邦 | GitLab 用户/组管理 | OAuth 同意屏幕 User type = Internal + `hd` 域 |
| issuer 形态 | 全局，或组织级路径 `/orgs/<名称>`（v4.26.0+） | realm 路径 `/realms/<realm>` | 源站本身 | 固定 accounts.google.com |
| 单点登出 | Holon 登出仅结束本地会话 | 同左（discovery 有 end_session_endpoint 但 Holon 不调用） | 同左 | 同左 |
| 刷新令牌 | 应用 grantTypes 含 refresh_token 即签发；token 有效期 `expireInHours` 可配（默认 168h） | 默认签发，1800s | 默认 2h（可配） | 需显式申请 offline_access |
| secret 语义 | 「提供即必须正确」；留空 + PKCE 按 public client 放行 | 标准 confidential client | 标准 OAuth 应用 | 标准 OAuth 客户端 |

## 本文所述的验证程度

以下结论来自 2026-11-08 的实测（Casdoor v4.26.0 官方 Docker 镜像 + MySQL 8.0.46 + nginx TLS sidecar + 隔离 Holon 实例，全局与组织路径 issuer 各验证一套）：

- 已实测：程序化 code flow 全链（PKCE S256、state、nonce）在 Casdoor 与隔离 Holon 实例之间完成真实登录并建立会话（登录请求与 Casdoor React 前端调用相同，`/api/auth/session/me` 返回 `auth_method=oidc`，登出后 401）；全局 issuer 与组织路径 issuer（`/orgs/<名称>`）两套完整流程均通过，Holon 对路径形 discovery 直接接受，无 issuer 不匹配问题；反向用例——白名单外 redirect_uri 不发授权码、错误 client_id 拒绝、错误 client_secret 返回 401 `invalid_client`、轮换 secret 后旧凭据立即失效、停用用户后无法再建立会话——全部正确拒绝。
- 已知行为（非阻塞发现）：Holon 对无效的 OIDC 回调（state/浏览器会话不匹配）能正确拒绝且不建立会话，但返回 500 `runtime_error` 而非 4xx（holon-run/holon#3882），跨 GitLab/Keycloak/Casdoor 三个 provider 均可复现；不影响功能正确性。
- 未实测：浏览器手动点击的完整 UI 渲染流程（验证以程序化 HTTP 流为准，登录请求与前端一致）、v4.26.0 之前的 Casdoor 版本、SAML/LDAP/SCIM 与外部联邦、全局与组织路径两个 issuer 的 JWKS 差异比对、生产规模部署与备份恢复。
