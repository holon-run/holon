package run.holon.android.app

import java.net.URI

internal fun normalizeScannedAddress(input: String): String {
    val trimmed = input.trim().trimEnd('/')
    require(trimmed.length <= 2048) { "二维码地址过长" }
    require(trimmed.isNotEmpty()) { "二维码不包含地址" }
    val uri = runCatching { URI(trimmed) }.getOrElse {
        throw IllegalArgumentException("二维码地址格式无效")
    }
    val scheme = uri.scheme.lowercase()
    require(scheme == "http" || scheme == "https") {
        "二维码地址必须使用 HTTP 或 HTTPS"
    }
    require(uri.host != null && uri.userInfo == null && uri.query == null && uri.fragment == null) {
        "二维码地址不能包含凭据、查询参数或片段"
    }
    require(uri.path.isEmpty() || uri.path == "/api") {
        "二维码地址路径只能为空或 /api"
    }
    return URI(
        scheme,
        null,
        uri.host,
        uri.port,
        uri.path.ifEmpty { null },
        null,
        null,
    ).toString().trimEnd('/')
}
