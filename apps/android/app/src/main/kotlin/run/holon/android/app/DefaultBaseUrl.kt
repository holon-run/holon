package run.holon.android.app

internal fun defaultBaseUrl(): String =
    if (BuildConfig.DEBUG) {
        "http://10.0.2.2:7878/api"
    } else {
        "https://holon.run/api"
    }
