package run.holon.android.app

import java.net.URI

internal data class ScannedPairing(val address: String, val ticket: String)

internal fun parseScannedPairing(input: String): ScannedPairing {
    val trimmed = input.trim()
    require(trimmed.length <= 2048) { "配对二维码过长" }
    val uri = runCatching { URI(trimmed) }.getOrElse {
        throw IllegalArgumentException("配对二维码格式无效")
    }
    require(uri.scheme?.lowercase() in setOf("http", "https") &&
        uri.host != null && uri.userInfo == null && uri.rawQuery == null &&
        uri.path == "/login" && uri.port in -1..65535) {
        "配对二维码必须是 Holon 登录地址"
    }
    val ticket = uri.rawFragment?.removePrefix("pair=")
    require(uri.rawFragment?.startsWith("pair=") == true &&
        ticket?.matches(Regex("[0-9a-fA-F]{64}")) == true) {
        "配对二维码票据无效"
    }
    return ScannedPairing(
        URI(uri.scheme.lowercase(), null, uri.host, uri.port, null, null, null).toString(),
        ticket,
    )
}

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
