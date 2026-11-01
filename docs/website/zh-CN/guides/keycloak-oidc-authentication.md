---
title: 用自托管 Keycloak 配置 OIDC 单点登录
summary: 自托管 Keycloak 作为 Holon 的 OIDC 身份提供商：Docker 部署、创建 realm 与客户端、配置 Holon 并完成端到端验证，全程不依赖任何外部账号。
order: 187
---

# 用自托管 Keycloak 配置 OIDC 单点登录

Keycloak 是开源身份与访问管理平台，可以作为 Holon 的 OIDC 身份提供商：用户、客户端、会话策略全部在你自己的 Keycloak 里管理，不需要向任何第三方注册。相比「顺带提供 OAuth 应用」的 GitLab，Keycloak 是专职 IdP，适合作为组织统一的登录入口。本文给出从部署到端到端验证的完整流程，全部步骤在一台机器上即可复现。

通用概念（Issuer、Client ID、回调地址、会话 TTL、审计归属）见[配置 OIDC 认证](./configure-oidc-authentication.md)。本文只补充自托管 Keycloak 特有的部分。

## 前置条件

- 一台可以跑 Docker 的主机（Keycloak 生产模式容器约需 1 GB 内存，远低于 GitLab）。
- Keycloak 站点必须以 HTTPS 对外提供。Holon 的 OIDC 客户端只信任内置的 Mozilla 根证书，**自签名 CA 不可行**；内网环境推荐用 Tailscale 的 `tailscale cert` 签发受公信 CA（Let's Encrypt）背书的证书，无需公网暴露端口。
- 一个可访问的 Holon 实例。回调地址允许 HTTPS，或主机名严格为 `localhost` 的 HTTP（端口任意）。`127.0.0.1`、`::1` 这类 IP 字面量会被拒绝。

## 第 1 步：部署 Keycloak

用生产模式（`start`）启动，把证书挂载进容器。`KC_HOSTNAME` 必须写**完整对外 URL（含端口）**，否则 discovery 文档里的 `issuer`、端点地址会缺端口，Holon 校验 issuer 时会失败：

```bash
mkdir -p keycloak/certs
cp site.crt site.key keycloak/certs/
docker run -d --name keycloak \
  -p 9444:8443 \
  -v "$PWD/keycloak/certs/site.crt:/opt/keycloak/conf/certs/tls.crt:ro" \
  -v "$PWD/keycloak/certs/site.key:/opt/keycloak/conf/certs/tls.key:ro" \
  -e KC_BOOTSTRAP_ADMIN_USERNAME=admin \
  -e KC_BOOTSTRAP_ADMIN_PASSWORD="$(openssl rand -hex 16)" \
  -e KC_HOSTNAME=https://keycloak.example.com:9444 \
  -e KC_HTTPS_CERTIFICATE_FILE=/opt/keycloak/conf/certs/tls.crt \
  -e KC_HTTPS_CERTIFICATE_KEY_FILE=/opt/keycloak/conf/certs/tls.key \
  quay.io/keycloak/keycloak:26.3 \
  start
```

说明：

- 证书文件默认挂载为 root 只读，而容器内 Keycloak 以 uid 1000 运行。宿主机上执行 `chown 1000 keycloak/certs/site.key`（或确保 uid 1000 可读）后再启动。
- `KC_BOOTSTRAP_ADMIN_USERNAME` / `KC_BOOTSTRAP_ADMIN_PASSWORD` 是 Keycloak 26+ 的引导管理员变量（替代旧的 `KEYCLOAK_ADMIN`）。
- `start-dev`（HTTP 开发模式）不适合本场景：Holon 校验 issuer 为 HTTPS，生产部署也必须 TLS。
- 就绪判定：`curl https://keycloak.example.com:9444/realms/master/.well-known/openid-configuration` 返回 200，且证书链验证通过（不带 `-k`）。

## 第 2 步：创建 realm、客户端与用户

登录管理控制台（`https://keycloak.example.com:9444/`，用引导管理员账号），或用 Admin REST API：

1. **创建 realm**：左上角下拉 → **Create realm** → 名称如 `holon-test`。realm 是隔离的用户与客户端命名空间，生产上建议为 Holon 单独建一个 realm。
2. **创建客户端**：realm 内 **Clients → Create client**：
   - Client type 保持 **OpenID Connect**；Client ID 如 `holon-rp-test`。
   - 打开 **Client authentication**（即机密/confidential 客户端）。
   - 勾选 **Standard flow**；关闭 Direct access grants（可选，更收敛）。
   - **Valid redirect URIs** 填 Holon 的回调地址，例如 `http://localhost:7880/api/auth/oidc/callback`。
3. **记录客户端密钥**：Clients → `holon-rp-test` → **Credentials** 标签页复制 Client secret。
4. **创建用户**：**Users → Add user**，填用户名与邮箱（勾选 Email verified）；在 **Credentials** 标签页 **Set password**，并把 **Temporary 关掉**——临时密码会强制首次登录改密，自动化与部分嵌入式登录流程会因此卡住。

对应的 REST API 配置脚本（可复现，凭据从文件读取，不回显密钥）：

```bash
BASE=https://keycloak.example.com:9444
TOKEN=$(curl -s "$BASE/realms/master/protocol/openid-connect/token" \
  -d grant_type=password -d client_id=admin-cli \
  -d username=admin -d password="$ADMIN_PASS" | jq -r .access_token)
curl -s -X POST "$BASE/admin/realms" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"realm":"holon-test","enabled":true,"sslRequired":"external","registrationAllowed":false}'
curl -s -X POST "$BASE/admin/realms/holon-test/clients" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"clientId":"holon-rp-test","enabled":true,"protocol":"openid-connect","publicClient":false,
       "standardFlowEnabled":true,"directAccessGrantsEnabled":false,
       "redirectUris":["http://localhost:7880/api/auth/oidc/callback"]}'
curl -s -X POST "$BASE/admin/realms/holon-test/users" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"username":"alice","enabled":true,"email":"alice@example.com","emailVerified":true}'
# 密码：PUT /admin/realms/holon-test/users/<uuid>/reset-password，body 里 temporary:false
```

## 第 3 步：配置 Holon

```bash
holon config set auth.oidc.issuer_url https://keycloak.example.com:9444/realms/holon-test
holon config set auth.oidc.client_id holon-rp-test
holon config set auth.oidc.client_secret_env KEYCLOAK_OIDC_CLIENT_SECRET
holon config set auth.oidc.redirect_uri http://localhost:7880/api/auth/oidc/callback
holon config set auth.mode oidc
```

两个容易踩的坑：

- `issuer_url` 末尾必须精确到 **`/realms/<realm 名>`**，与 Keycloak discovery 的 `issuer` 完全一致，多斜杠或 realm 名大小写不一致都会失败。
- `KEYCLOAK_OIDC_CLIENT_SECRET` 是环境变量名，密钥值在启动 Holon 时注入，不写入配置文件：

```bash
KEYCLOAK_OIDC_CLIENT_SECRET="<Client Secret>" holon serve --port 7880
```

已知限制（与 GitLab 相同）：当前版本 `holon config set` 每次写入后都会对整份认证配置做校验，因此**先设置 `auth.mode oidc` 再补 `auth.oidc.*` 子键会被拒绝**。按本文顺序（先子键后 mode）可以避开；如果已经设置过 mode，直接编辑 `config.json` 后重启即可。

## 第 4 步：验证

1. 打开 Holon 登录页，应出现「Continue with organization login」；点击后跳转 Keycloak realm 的登录页（URL 含 `/realms/<realm>/`）。
2. 登录 Keycloak 用户，回到 Holon 后检查会话：

```bash
curl -s http://localhost:7880/api/auth/session/me
# {"ok":true,"user_id":"oidc-…","display_name":"…","auth_method":"oidc"}
```

3. 登出后同一路径应返回 401：

```bash
curl -s -X POST http://localhost:7880/api/auth/session/logout
```

注意：Holon 登出只结束**本地**会话。Keycloak 浏览器会话（SSO）仍然有效，同一浏览器再次发起授权时无需重新输密码即可拿到新授权码。这是当前版本的预期行为，与 GitLab、Google Workspace 一致；Keycloak 虽然在 discovery 中提供 `end_session_endpoint`（RP-initiated logout 规范端点），Holon 当前不调用它。组织需要「登出即全端下线」的场景，目前需要靠缩短 Keycloak 的 SSO Session Idle 时长兜底。

反向检查：

- 错误 client_secret：token 端点返回 401。注意 Keycloak 的错误码是 `unauthorized_client`（GitLab 用 `invalid_client`，Google 用 `invalid_client`），拒绝行为一致但错误码不同。
- 未注册的 redirect_uri：authorize 直接返回 400 错误页，拿不到授权码。
- scope 缺 `openid`：token 响应没有 `id_token`（Keycloak 默认会签发 refresh_token，`refresh_expires_in` 默认 1800 秒）。
- 篡改 id_token 签名：本地用 JWKS 公钥验签会失败。

## 与 GitLab CE / Google Workspace 的差异

| 维度 | 自托管 Keycloak | 自托管 GitLab CE | Google Workspace |
| --- | --- | --- | --- |
| 外部依赖 | 无（全部本地） | 无（全部本地） | 需要 Google Cloud 项目与组织 |
| IdP 定位 | 专职身份平台，realm 级隔离 | DevOps 平台内置 OAuth | 云办公套件身份 |
| 组织准入 | realm 内用户管理；可接 LDAP/SAML 联邦 | GitLab 用户/组管理 | OAuth 同意屏幕 User type = Internal + `hd` 域（Holon 侧不校验 `hd`） |
| 单点登出 | discovery 有 `end_session_endpoint`，但 Holon 当前不调用，登出仅结束本地会话 | discovery 无该端点，同样仅本地登出 | 同样不由 Holon 强制 |
| 刷新令牌 | 默认签发，`refresh_expires_in` 1800s | 默认 2h（可配） | 需显式申请 `offline_access`（access_type=offline） |
| 首登强制改密 | 用户密码带 Temporary 标记时会卡住自动化流程（创建用户时务必关掉） | 无此机制 | 无此机制 |

## 本文所述的验证程度

以下结论来自 2026-11-01 的实测（Keycloak 26.3.5 官方 Docker 镜像 + 隔离 Holon 实例，issuer 为 `https://<tailscale 主机名>:9444/realms/holon-test`）：

- 已实测：程序化 code flow 全链（PKCE S256、state、nonce）在 Keycloak 与隔离 Holon 实例之间完成真实表单登录并建立会话（`/api/auth/session/me` 返回 `auth_method=oidc`，登出后 401）；id_token 通过本地重建 JWKS 公钥完成 RS256 验签，`iss`/`aud`/`exp`/`iat`/`nonce`/`sub`/`at_hash` 合规，`preferred_username`/`email` claims 正确，userinfo 的 `sub` 一致；refresh_token 默认签发且可换新 access_token；错误 client_secret 被拒（401 `unauthorized_client`）、未注册 redirect_uri 返回 400、缺 `openid` scope 无 id_token、篡改签名被本地验签拒绝、撤销 access_token 后 userinfo 401；Holon 登出后 Keycloak SSO 会话保留（再次授权免密直通）。
- 已知行为（非阻塞发现）：Holon 对无效的 OIDC 回调（state/浏览器会话不匹配）能正确拒绝且不建立会话，但返回 500 `runtime_error` 而非 4xx；如需更规范的错误语义，建议关注 Holon 的后续版本。
- 未实测：浏览器手动点击的完整 UI 流程（验证以程序化 HTTP 流为准）、realm 联邦（LDAP/AD/SAML）、用户组 claims（`groups` mapper）、RP-initiated logout 接入、外部数据库（默认内置 H2 仅适合评估）、生产规模的部署与备份恢复。
