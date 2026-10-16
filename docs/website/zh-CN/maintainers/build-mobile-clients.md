---
title: 构建移动客户端
summary: 从本地代码检出构建并安装 Android 与 iOS 原生客户端。
---

# 构建移动客户端

本页供开发移动客户端或测试本地改动时使用。配对连接与日常使用操作，请参阅 [Android 客户端指南](../guides/connect-android-client.md) 或 [iOS 客户端指南](../guides/connect-ios-client.md)。

## 前置条件

- 完整的 Holon 仓库源码检出。
- 正在运行且能够从真机或模拟器网络访问的 Holon 守护进程。
- Android 环境：JDK 21、Android SDK 以及用于真机部署的 `adb` 命令行工具。
- iOS 环境：配备 Xcode 的 macOS 系统，以及兼容 iOS 18 的模拟器或真机。部署到真机时需在 Xcode 中配置签名。

除特别注明外，以下命令均在仓库根目录执行。

## Android

构建 Debug 版本的 APK 并安装到已连接的真机或模拟器：

```bash
cd apps/android
./gradlew :app:assembleDebug
adb install app/build/outputs/apk/debug/app-debug.apk
```

对于通过 USB 连接的真机，可反向代理守护进程的端口：

```bash
adb reverse tcp:7878 tcp:7878
```

Android 模拟器可通过 `http://10.0.2.2:7878` 访问宿主机；在开启 USB 端口转发后，真机可以直接通过 `http://127.0.0.1:7878` 访问。

打开安装好的 App，完成[连接与结果核对](../guides/connect-android-client.md#验证连接与交付结果)。单靠 Gradle 编译成功，并不能证明设备端网络连通或登录流程无误。

## iOS

打开 Xcode 工程：

```bash
open apps/ios/Holon.xcodeproj
```

选择目标真机或模拟器，使用 `Cmd + R` 运行工程。对于真机调试，连接地址应使用宿主机的局域网 IP 或 Tailnet 地址；手机上的 `localhost` 指向手机自身。

按照客户端指南中的说明完成[连接与文件访问验证](../guides/connect-ios-client.md#验证结果与文件访问)。在模拟器中能够运行，并不代表真机、TestFlight 或 App Store 审核已经就绪。

## 故障排查

- **Android 在编译前报错：** 核对 JDK 与 Android SDK 配置是否与仓库中的 `apps/android/README.md` 一致。
- **`adb` 无法安装应用：** 检查目标设备是否已连接并开启 USB 调试授权。在覆盖安装前先解决包名或签名冲突；卸载现有应用会清空移动端的本地数据。
- **Xcode 无法部署到真机：** 检查当前选择的 Target、支持的系统版本以及签名证书配置。切勿将签名证书私钥提交到日志或版本控制中。
- **应用能够打开但无法连接：** 遵循对应平台的连接排查指南。构建成功与运行时网络可达属于独立的验证步骤。

关于正式版本的签名与分发规范，请查阅仓库内的 `apps/android/RELEASING.md` 与 `apps/ios/RELEASE.md`。本地 Debug 构建步骤不负责生成或发布应用商店包。
