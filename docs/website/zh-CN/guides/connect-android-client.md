---
title: 连接 Android 客户端
summary: 配置并连接原生 Android 客户端，在移动设备上管理 Holon Agent 与查看任务交付物。
order: 37
---

# 连接 Android 客户端

Holon 提供了基于 Android SDK 构建的原生 Android 客户端。客户端专为移动端日常使用设计，
采用以 Brief 为先（Brief-First）的工作区布局，方便你随时在手机、平板或模拟器上查阅任务交付物、浏览工作区文件并派发任务。

本指南介绍如何将 Android 客户端连接到运行中的 Holon 守护进程。

## 前置条件

- 已启动并启用 HTTP 控制平面的 Holon 守护进程（默认端口 `7878`）。
- 运行 Android 8.0（API 级别 26）或更高版本的 Android 物理设备或模拟器。
- 如通过 USB 调试连接物理设备：开发机已安装 `adb` 命令行工具。

## 连接网络方案

根据测试环境选择对应的连接方式：

| 运行环境 | 默认 API Base URL | 开发机准备操作 |
|---------------------|----------------------|------------------|
| Android 官方模拟器 | `http://10.0.2.2:7878/api` | 无需额外操作 |
| USB 物理设备 | `http://127.0.0.1:7878/api` | 执行 `adb reverse tcp:7878 tcp:7878` |
| 局域网 / 远程服务器 | `https://holon.example.com/api` | 配置反向代理或 Tailnet |

> **安全提醒：** 生产环境应始终采用 HTTPS。虽然 Debug 构建允许直接连接本地回环 HTTP，但在跨网络连接未加密的 HTTP 时，应用界面会弹出显式风险确认。

## 第一步：准备守护进程

确认 Holon 守护进程已正常运行并监听指定接口。模拟器或本地 USB 测试时，监听 localhost 即可：

```bash
holon daemon start
```

如果守护进程开启了认证，准备好访问凭据。这可以是启动守护进程时通过
`--token <TOKEN>` 或 `--token-file <PATH>` 指定的 Bearer Token，也可以是初始化引导流程生成的
Bootstrap Token。

## 第二步：配置 USB 端口反向代理（物理机测试）

如果使用 USB 数据线连接手机测试，将手机端的端口流量转发到开发机：

```bash
adb reverse tcp:7878 tcp:7878
```

使用标准 Android 模拟器时无需执行此步骤。

## 第三步：在 Android 应用中登录

1. 在设备上打开 Holon 应用。
2. 输入 **API Base URL**：
   - 模拟器：`http://10.0.2.2:7878/api`
   - USB 转发设备：`http://127.0.0.1:7878/api`
   - 远程服务器：`https://<你的域名>/api`
3. 输入认证 Token。
4. 点击 **连接**。

客户端会向服务端的 `/api/auth/session/exchange/native` 端点请求兑换，将生成的会话安全存入系统底层的 Android Keystore，同时从内存中抹除原始 Token。

## 第四步：在移动端与 Agent 交互

成功连接后，移动工作区为你提供以下核心能力：

- **以 Brief 为先的工作区：** 核心聚焦于高信息密度的最终结论。应用优先突出完成简报（Brief）、进行中的工作项与待办进度，无需翻阅冗长的底层执行链路即可掌握任务现状。
- **实时同步的 Agent 列表：** 查看所有持久化 Agent、当前状态（清醒/休眠）及活跃子 Agent。列表通过服务端实时事件自动增量同步，无需手动下拉刷新。
- **文件阅读器与消息跳转：** 原生支持在内置阅读器中查看计划文档、Markdown 笔记和工作区文件。当 Agent 在对话中输出文件路径时，点击链接即可直接在设备上打开查看。
- **具备持久化发件箱的输入框：** 提供体验流畅的多行任务输入框。在网络波动或离线时，输入的指令会自动存入本地发件箱，待连接恢复后自动同步发送。

## 界面语言设置

Android 客户端界面原生支持**英语**与**简体中文**。

应用默认匹配系统设备或 Android 每应用偏好语言。你也可以在登录界面或**设置**页面中手动选择偏好语言（系统默认、English 或 简体中文）。该配置持久保存在当前设备中，退出登录后依然有效。

## 从源码编译客户端

如需自行从源码构建 APK：

```bash
cd apps/android
./gradlew :app:assembleDebug
```

编译完成后执行 `adb install app/build/outputs/apk/debug/app-debug.apk` 安装到设备。

## 另请参阅

- [远程连接指南](/zh-CN/guides/connect-remote-runtime.md)：远程守护进程的安全网络配置。
- [OIDC 单点登录指南](/zh-CN/guides/configure-oidc-authentication.md)：统一身份认证与会话策略配置。
- [HTTP 控制平面参考](/zh-CN/reference/http-control-plane.md)：Android SDK 底层对接的 HTTP 端点说明。
