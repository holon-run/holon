package run.holon.android.app

import android.os.Build

internal fun defaultBaseUrl(): String =
    if (BuildConfig.DEBUG) {
        if (isEmulator()) {
            "http://10.0.2.2:7878/api"
        } else {
            "http://127.0.0.1:7878/api"
        }
    } else {
        "https://holon.run/api"
    }

private fun isEmulator(): Boolean =
    Build.FINGERPRINT.startsWith("generic") ||
        Build.FINGERPRINT.contains("emulator") ||
        Build.MODEL.contains("Emulator") ||
        Build.MODEL.contains("Android SDK built for") ||
        Build.PRODUCT.contains("sdk_gphone")
