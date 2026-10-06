---
title: 连接 Android 客户端
summary: 安装并配置原生 Android 客户端，通过扫码配对连接运行中的 Holon 守护进程，在移动端管理 Agent 与对话。
order: 37
---

# 连接 Android 客户端

Holon 提供了专为移动端工作流设计的原生 Android 客户端。应用以对话为核心，支持查看完成简报（Brief）、本地会话缓存以及与守护进程一键配对。

本指南介绍应用安装、连接配置、网络配置多环境管理以及移动端核心功能。

## 前置条件

- 已启动并开启 HTTP 的 Holon 守护进程（默认端口 `7878`）。
- 运行 Android 8.0（API 级别 26）或更高版本的设备或模拟器。
- 本地 USB 调试需安装 `adb` 命令行工具。

## 安装客户端

### 下载官方安装包

直接从 [Holon GitHub Releases](https://github.com/holon-run/holon/releases) 页面下载官方签名的发布包。每个版本提供用于设备直接安装的 `holon-android-v<version>.apk`，以及用于 Google Play 分发的 `holon-android-v<version>.aab`（Bundle 格式不可直接通过 `adb` 安装）。

通过 `adb` 将 APK 安装到设备：

```bash
adb install holon-android-v0.48.0.apk
```

### 从源码编译

也可以自行从源码编译 Debug 版本：

```bash
cd apps/android
./gradlew :app:assembleDebug
adb install app/build/outputs/apk/debug/app-debug.apk
```

## 连接网络方案

| 运行环境 | 默认 API Base URL | 宿主机准备操作 |
|---------------------|----------------------|------------------|
| Tailscale / 局域网 | `https://<tailnet-host>/api` 或 `http://<lan-ip>:7878/api` | 开启 Tailscale Serve 或绑定局域网接口 |
| 官方模拟器 | `http://10.0.2.2:7878/api` | 使用默认配置 |
| USB 物理设备 | `http://127.0.0.1:7878/api` | 运行 `adb reverse tcp:7878 tcp:7878` |

> **安全说明：** 生产环境建议使用 HTTPS。Tailscale Serve 可直接为你的私有网络提供自动化证书。跨网络使用未加密的 HTTP 时，客户端会提示显式风险确认。

## 连接到守护进程

### 方式一：二维码配对（推荐）

1. 在浏览器中打开 Holon Web GUI，进入 **Settings** -> **Device Pairing**（或点击 macOS 菜单中的 **Pair Device**）。
2. 打开 Holon Android 应用，点击 **扫码连接**。
3. 扫描页面上的 2 分钟有效一次性二维码。

应用会自动请求 `/api/auth/pairing/redeem/native` 将一次性票据兑换为 Session 凭据，安全保存在 Android Keystore 中并完成连接。

### 方式二：手动配置

1. 打开 Holon 应用。
2. 输入 **API Base URL**（如 `http://10.0.2.2:7878/api` 或 `https://<你的域名>/api`）。
3. 输入控制令牌（Bearer Token）或 Bootstrap Token。
4. 点击 **连接**。

客户端请求 `/api/auth/session/exchange/native` 兑换可撤销的会话凭据，存入 Android Keystore 并从内存中抹除原始 Token。

### 方式三：OIDC 单点登录（SSO）

若守护进程开启了 OIDC 认证：

1. 打开 Holon 应用并输入 **API Base URL**。
2. 点击 **通过 SSO 登录**。
3. 应用调起系统浏览器完成身份提供商登录。
4. 登录成功后，浏览器自动通过 `run.holon.android://oidc/callback` 重定向唤醒应用。

应用将一次性 bootstrap 票据兑换为可撤销的会话凭据，存入当前网络配置对应的 Android Keystore 中并完成连接。

## 管理网络配置（Network Profiles）

应用支持在**设置**中保存多个连接配置（例如“家中 Tailscale”、“办公室局域网”与“本机 USB”）。保存后可一键切换网络环境，无需重复输入地址和凭据。

## 移动端核心功能

- **对话核心与本地历史缓存：** 会话历史在设备本地自动缓存，即使在弱网或重连状态下也能立即浏览既往对话与交付内容。
- **日常导航与任务卡片：** 支持按天查看活动时间线，直观展示后台任务进度卡片，并可直接在移动端查看子 Agent 的实时状态预览。
- **以 Brief 为先的工作区：** 优先突出最终完成简报、活动工作项与待办清单，无需查看冗长的执行细节即可确认成果。
- **模型切换：** 在移动端直接查看并覆盖单个 Agent 所使用的模型。
- **系统分享集成：** 支持从其他 Android 应用中通过系统分享面板将文本、链接或文档直接发送给指定的 Agent。
- **本地持久化发件箱：** 离线时输入的指令会自动存入本地发件箱，网络恢复后自动提交发送。
- **脱敏诊断日志：** 在设置中可导出脱敏环形缓冲区追踪日志，便于排查连接与同步问题。

## 界面语言设置

应用支持英语和简体中文。默认遵循系统语言设置，也可以在登录界面或**设置**中手动指定。

## 下一步

- [远程连接指南](/zh-CN/guides/connect-remote-runtime.md) — 远程守护进程的安全网络配置。
- [OIDC 身份认证指南](/zh-CN/guides/configure-oidc-authentication.md) — 统一身份认证与会话策略。
- [HTTP 控制平面参考](/zh-CN/reference/http-control-plane.md) — Android 客户端使用的底层端点说明。
