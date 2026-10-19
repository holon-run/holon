package run.holon.android.app

import android.annotation.SuppressLint
import android.content.Context
import androidx.webkit.Profile
import androidx.webkit.ProfileStore
import androidx.webkit.WebStorageCompat
import androidx.webkit.WebViewFeature
import java.util.UUID

internal const val RUNTIME_BROWSER_PROFILE_PREFIX = "holon_runtime_browser_"

/** Loaded profiles cannot be deleted in this process. Names are never reused. */
@SuppressLint("RequiresFeature")
internal object RuntimeBrowserProfiles {
    private var initialized = false

    fun isSupported(): Boolean =
        WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE) &&
            WebViewFeature.isFeatureSupported(WebViewFeature.DELETE_BROWSING_DATA)

    fun cleanupOnColdStart(context: Context) {
        if (initialized || !isSupported()) return
        val store = ProfileStore.getInstance()
        val pending = context.applicationContext.getSharedPreferences("runtime_browser_cleanup", Context.MODE_PRIVATE)
        // Do not load old profiles before deleting them; getProfile would prevent deletion.
        store.allProfileNames.filter { it.startsWith(RUNTIME_BROWSER_PROFILE_PREFIX) }.forEach { name ->
            check(store.deleteProfile(name)) { "旧网页数据清理失败，请重启应用后重试" }
            pending.edit().remove(name).apply()
        }
        initialized = true
    }

    fun create(context: Context): RuntimeBrowserProfile {
        check(isSupported()) { "当前 Android System WebView 不支持安全的身份隔离与清理，请升级 WebView，或在系统浏览器另行登录。" }
        cleanupOnColdStart(context)
        val store = ProfileStore.getInstance()
        val pending = context.applicationContext.getSharedPreferences("runtime_browser_cleanup", Context.MODE_PRIVATE)
        val name = RUNTIME_BROWSER_PROFILE_PREFIX + UUID.randomUUID().toString()
        pending.edit().putString(name, "active").apply()
        return RuntimeBrowserProfile(store.getOrCreateProfile(name)) { status ->
            pending.edit().putString(name, status).apply()
        }
    }
}

@SuppressLint("RequiresFeature")
internal class RuntimeBrowserProfile(
    val profile: Profile,
    private val record: (String) -> Unit,
) {
    var retired = false
        private set
    var cleanupComplete = false
        private set

    /** Call only after destroying every WebView for this lease. No success before callback. */
    fun retire() {
        if (retired) return
        retired = true
        record("pending")
        runCatching {
            WebStorageCompat.deleteBrowsingData(profile.webStorage) {
                cleanupComplete = true
                record("cleared")
            }
        }.onFailure { record("failed") }
    }
}
