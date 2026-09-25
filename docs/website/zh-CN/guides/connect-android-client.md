---
title: 连接 Android 客户端
summary: 配置并连接原生 Android 客户端，在移动设备上管理 Holon Agent 与查看任务交付物。
order: 37
---

# 连接 Android 客户端

从 v0.45.0 开始，Holon 提供了基于 Jetpack Compose 与全新 Android SDK 构建的原生 Android 客户端。你可以通过移动设备或模拟器随时查看 Agent 状态、追踪工作流进展并提交任务。

本指南介绍如何将 Android 应用连接到运行中的 Holon 守护进程。

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

如果守护进程开启了认证，准备好访问凭据（Bearer Token 或临时 Bootstrap Token）：

```bash
holon config get auth.token
```

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

客户端会向服务端的 `/api/auth/session` 端点请求兑换，将生成的会话安全存入系统底层的 Android Keystore，同时从内存中抹除原始 Token。

## 第四步：在移动端与 Agent 交互

完成连接后，你可以在应用中：

- **浏览 Agent 列表：** 查看所有持久化 Agent 身份、当前运行状态（清醒/休眠）及活跃子 Agent。
- **跟踪任务与交付物：** 查看进行中的工作项、待办清单以及最终产出的 Brief 报告。
- **提交提示词与任务：** 直接向 Agent 发送新指令。应用包含持久化的离线发件箱（Outbox），网络中断或微弱时输入的指令会自动暂存，并在重连后恢复发送。
- **查看文件附件与生成内容：** 在对话流中直接阅读 Markdown 总结、文本记录与生成的静态资源。

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
