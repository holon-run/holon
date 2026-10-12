package run.holon.android.app

import run.holon.android.sdk.HolonHttpException
import run.holon.android.sdk.HolonProtocolException

internal fun authenticationMethodHumanError(error: Throwable): String =
    when {
        error is HolonHttpException && error.statusCode in setOf(401, 403) ->
            "服务器拒绝了登录方式查询，请检查服务器配置"
        error is HolonProtocolException &&
            (error.cause == null || error.cause is kotlinx.serialization.SerializationException) ->
            "服务器登录方式不受支持，请检查 Holon 版本和服务器配置"
        else -> humanError(error)
    }

internal fun HolonUiState.forConnectionAddress(
    address: String = baseUrl,
    allowHttp: Boolean = allowInsecureHttp,
): HolonUiState =
    copy(
        baseUrl = address,
        allowInsecureHttp = allowHttp,
        token = "",
        showToken = false,
        authenticationMethod = null,
        browserLoginUrl = null,
        pendingPairing = null,
        error = null,
        statusMessage = null,
    )
