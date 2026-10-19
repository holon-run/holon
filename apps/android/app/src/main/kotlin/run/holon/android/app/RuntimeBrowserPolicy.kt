package run.holon.android.app

import java.net.URI
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

/** Compare origins, not cookie domains: cookies do not isolate ports. */
internal fun sameRuntimeOrigin(url: String, baseUrl: String): Boolean {
    val target = safeBrowserHttpUrl(url) ?: return false
    val base = safeBrowserHttpUrl(baseUrl) ?: return false
    return target.scheme == base.scheme && target.host == base.host && target.port == base.port
}

internal fun safeBrowserHttpUrl(url: String): HttpUrl? {
    if (url.any { it.code <= 0x20 || it.code == 0x7f || it == '\\' }) return null
    val uri = runCatching { URI(url) }.getOrNull() ?: return null
    if (uri.rawAuthority?.contains('@') == true) return null
    return url.toHttpUrlOrNull()?.takeIf { it.username.isEmpty() && it.password.isEmpty() }
}

internal fun runtimeSessionCookie(credential: String, url: String): String =
    "holon_session=$credential; Path=/; HttpOnly; SameSite=Lax" +
        if (safeBrowserHttpUrl(url)?.isHttps == true) "; Secure" else ""

internal enum class RuntimeBrowserNavigation { Internal, External, Blocked }

internal fun runtimeBrowserNavigation(url: String, baseUrl: String): RuntimeBrowserNavigation {
    if (safeBrowserHttpUrl(url) == null) return RuntimeBrowserNavigation.Blocked
    return if (sameRuntimeOrigin(url, baseUrl)) RuntimeBrowserNavigation.Internal else RuntimeBrowserNavigation.External
}
