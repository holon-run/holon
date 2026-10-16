---
title: Build mobile clients
summary: Build and install the Android and iOS clients from a local checkout.
---

# Build mobile clients

Use this page when developing a mobile client or testing a local change.
For pairing and everyday use, follow the [Android](../guides/connect-android-client.md)
or [iOS](../guides/connect-ios-client.md) guide.

## Prerequisites

- A checkout of the Holon repository.
- A running daemon reachable from the device or simulator.
- For Android: JDK 21, an Android SDK, and `adb` for device installation.
- For iOS: a Mac with Xcode and an iOS 18-compatible simulator or device.
  Configure signing in Xcode when deploying to a physical device.

Run the commands below from the repository root unless stated otherwise.

## Android

Build the debug APK and install it on a connected device or emulator:

```bash
cd apps/android
./gradlew :app:assembleDebug
adb install app/build/outputs/apk/debug/app-debug.apk
```

For a USB-connected device, forward the daemon port:

```bash
adb reverse tcp:7878 tcp:7878
```

The Android emulator can reach the host at `http://10.0.2.2:7878`; with USB
forwarding, use `http://127.0.0.1:7878` on the connected device.

Open the installed app and complete the
[connection check](../guides/connect-android-client.md#verify-the-connection-and-a-result).
A successful Gradle build alone does not verify device networking or sign-in.

## iOS

Open the Xcode project:

```bash
open apps/ios/Holon.xcodeproj
```

Select a device or simulator and run the project with `Cmd + R`. For a physical
device, use the host's reachable LAN or tailnet address; `localhost` refers to
the phone.

Complete the [connection and file checks](../guides/connect-ios-client.md#verify-results-and-file-access)
in the client guide. A simulator run does not establish physical-device,
TestFlight, or App Store acceptance.

## Troubleshooting

- **Android build fails before compilation:** Check the JDK and Android SDK
  configuration against `apps/android/README.md` in the checkout.
- **`adb` cannot install:** Check that the intended device is connected and
  authorized. Resolve any package or signing conflict before replacing an
  existing installation; removal can erase local client data.
- **Xcode cannot deploy:** Check the selected target, supported OS, and signing
  configuration. Keep signing credentials out of logs and commits.
- **App opens but cannot connect:** Follow the platform's connection guide.
  Build success and runtime reachability are separate checks.

For release signing and distribution, consult `apps/android/RELEASING.md` and
`apps/ios/RELEASE.md` in the repository. These debug-build steps do not produce
or publish a store release.
